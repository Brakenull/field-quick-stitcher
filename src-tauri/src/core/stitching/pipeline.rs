//! The Quick Stitch pipeline end to end (spec section 5), independent of
//! Tauri - `commands::stitch::quick_stitch` is a thin wrapper around `run`
//! that reads the cached scan result and forwards progress over IPC. Split
//! out mainly so it's testable without a Tauri `AppHandle`, the same way
//! `pipeline_test.rs` exercises Phase 1's pipeline via `commands::inspect`'s
//! free functions.

use std::path::{Path, PathBuf};
use std::time::Instant;

use opencv::imgcodecs;
use opencv::prelude::*;

use crate::core::geometry;
use crate::models::photo_meta::{LonLat, PhotoMeta};
use crate::models::stitch_result::{StitchProgress, StitchResult, StitchStage};
use crate::utils::temp_workspace::TempWorkspace;
use crate::utils::thread_pool::par_map_with_progress;

use super::{downsample, geotiff_export, homography, mosaic, neighbor_index, onnx_matcher, onnx_matcher::OnnxMatcher, pose_graph};

const EARTH_RADIUS_M: f64 = 6_378_137.0;

/// Runs the full pipeline over `photos` (which must already be filtered to
/// only those with a computable footprint - GPS/yaw/altitude/focal/sensor),
/// writing the resulting GeoTIFF + PNG preview to paths derived from
/// `output_path`. `extractor_model_path`/`matcher_model_path` are the
/// standalone SuperPoint extractor and LightGlue matcher (see
/// `onnx_matcher.rs` for why this replaces both the earlier fused
/// SuperPoint+LightGlue graph and the ORB-extract-then-BFMatcher-match stages
/// `features.rs`/`matching.rs` still hold, dormant, for a possible future CPU
/// fallback path). `on_progress` is called throughout - each call names the
/// pipeline stage currently running plus a 0-100 percent *within* that stage
/// - so the caller can show real progress instead of a bar that stalls after
/// downsampling while feature extraction, matching, pose alignment, and (the
/// usually dominant) mosaic compositing run silently.
pub fn run(
    photos: Vec<PhotoMeta>,
    output_path: &Path,
    extractor_model_path: &Path,
    matcher_model_path: &Path,
    on_progress: impl Fn(StitchProgress) + Sync,
) -> Result<StitchResult, String> {
    let start = Instant::now();
    if photos.len() < 2 {
        return Err("Need at least 2 photos with full metadata (GPS/yaw/altitude/focal/sensor) to stitch.".to_string());
    }
    let report = |stage: StitchStage, percent: u8| on_progress(StitchProgress { stage, percent });

    let workspace = TempWorkspace::new().map_err(|e| e.to_string())?;
    let paths: Vec<PathBuf> = photos.iter().map(|p| PathBuf::from(&p.path)).collect();
    // Explicit 0% before each parallel stage below (this one included) - the
    // stage's own progress only ticks once the first ~10% of its items
    // finish, which on a slow machine can be silent for minutes; an upfront
    // 0% lets the UI show the stage has actually started rather than looking
    // stuck on the previous stage's 100%.
    report(StitchStage::Downsampling, 0);
    let downsampled = downsample::downsample_all(paths, &workspace, |p| report(StitchStage::Downsampling, p));

    let mut warnings = Vec::new();
    let mut usable_photos = Vec::with_capacity(photos.len());
    let mut cached_images = Vec::with_capacity(photos.len());
    for (photo, result) in photos.into_iter().zip(downsampled) {
        match result {
            Ok(cached) => {
                usable_photos.push(photo);
                cached_images.push(cached);
            }
            Err(e) => warnings.push(format!("{}: {e}", photo.file_name)),
        }
    }
    let photos = usable_photos;
    if photos.len() < 2 {
        return Err("Fewer than 2 photos could be loaded for stitching.".to_string());
    }
    // Photos dropped for actually lacking usable image data, as opposed to
    // the "weak visual alignment" advisories the matching stage appends to
    // the same `warnings` list below - captured now, before those join in.
    let photos_skipped = warnings.len();

    let image_paths: Vec<PathBuf> = cached_images.iter().map(|c| c.path.clone()).collect();
    let image_sizes: Vec<(f64, f64)> = cached_images.iter().map(|c| (c.width as f64, c.height as f64)).collect();

    let matcher = OnnxMatcher::load(extractor_model_path, matcher_model_path)?;

    // Extraction runs once per photo, directly on the downsampled cache
    // (see onnx_matcher.rs's doc comment for why that's safe for this
    // standalone extractor, unlike the earlier fused model) - image I/O runs
    // in parallel across photos, only the ONNX inference call itself
    // serializes (the extractor's session is behind a Mutex).
    report(StitchStage::DetectingFeatures, 0);
    let features: Vec<Result<onnx_matcher::FeatureSet, String>> =
        par_map_with_progress(image_paths.clone(), |path| matcher.extract(&path), |p| report(StitchStage::DetectingFeatures, p));
    let features: Vec<onnx_matcher::FeatureSet> = features.into_iter().collect::<Result<Vec<_>, String>>()?;

    let radius_m = neighbor_index::typical_radius_m(&photos);
    let pairs = neighbor_index::find_neighbor_pairs(&photos, radius_m);

    // Matching + homography estimation happen together per pair now (see
    // onnx_matcher.rs) - cheap relative to extraction since only cached
    // keypoint/descriptor tensors are involved, not pixels, but the matcher's
    // session is still behind a Mutex so inference calls serialize.
    report(StitchStage::MatchingPairs, 0);
    let pair_results: Vec<Result<Option<pose_graph::Edge>, String>> = par_map_with_progress(
        pairs,
        |(i, j)| -> Result<Option<pose_graph::Edge>, String> {
            let point_matches = matcher.match_pair(&features[i], &features[j])?;
            let correspondences: Vec<(opencv::core::Point2f, opencv::core::Point2f)> =
                point_matches.iter().map(|m| (m.a, m.b)).collect();
            let edge = homography::estimate(&correspondences).map_err(|e| e.to_string())?;
            Ok(edge.map(|h| pose_graph::Edge { from: i, to: j, h }))
        },
        |p| report(StitchStage::MatchingPairs, p),
    );
    let pair_results: Vec<Option<pose_graph::Edge>> = pair_results.into_iter().collect::<Result<Vec<_>, String>>()?;

    let mut edges = Vec::new();
    for edge in pair_results.into_iter().flatten() {
        if edge.h.inlier_count * 2 < edge.h.match_count {
            warnings.push(format!(
                "weak visual alignment between {} and {} ({}/{} inliers)",
                photos[edge.from].file_name, photos[edge.to].file_name, edge.h.inlier_count, edge.h.match_count
            ));
        }
        edges.push(edge);
    }
    let confident_pairs = edges.len();

    // Bounded by a hard iteration cap (see pose_graph::MAX_ITERATIONS), so
    // this never takes long enough to need incremental progress - just mark
    // the stage's start and end.
    report(StitchStage::AligningPoses, 0);
    let poses = pose_graph::align(&photos, &image_sizes, &edges).map_err(|e| e.to_string())?;
    report(StitchStage::AligningPoses, 100);

    let meters_per_pixel = average_meters_per_pixel(&photos, &image_sizes)
        .ok_or_else(|| "Could not determine a ground resolution for the mosaic.".to_string())?;

    report(StitchStage::Compositing, 0);
    let mosaic = mosaic::compose(&image_paths, &image_sizes, &poses, meters_per_pixel, |p| report(StitchStage::Compositing, p))
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "No photo could be placed in the mosaic.".to_string())?;

    report(StitchStage::Exporting, 0);
    let origin = pose_graph::world_origin(&photos);
    geotiff_export::write_geotiff(&mosaic, origin.0, origin.1, output_path).map_err(|e| e.to_string())?;

    let preview_path = output_path.with_extension("preview.png");
    imgcodecs::imwrite_def(&preview_path, &mosaic.image).map_err(|e| e.to_string())?;
    report(StitchStage::Exporting, 100);

    let preview_corners = mosaic_corners(&mosaic, origin);

    Ok(StitchResult {
        geotiff_path: output_path.to_string_lossy().to_string(),
        preview_path: preview_path.to_string_lossy().to_string(),
        preview_corners,
        photos_used: photos.len(),
        photos_skipped,
        confident_pairs,
        duration_ms: start.elapsed().as_millis(),
        warnings,
    })
}

/// Ground resolution for the output mosaic: the average meters-per-pixel
/// implied by each photo's own ground footprint at its (downsampled) size.
fn average_meters_per_pixel(photos: &[PhotoMeta], image_sizes: &[(f64, f64)]) -> Option<f64> {
    let mut sum = 0.0;
    let mut count = 0usize;
    for (photo, &(width_px, _)) in photos.iter().zip(image_sizes) {
        let (w_m, _) = geometry::ground_coverage_m(
            photo.relative_altitude?,
            photo.focal_mm?,
            photo.sensor_width_mm?,
            photo.sensor_height_mm?,
        );
        if w_m.is_finite() && w_m > 0.0 && width_px > 0.0 {
            sum += w_m / width_px;
            count += 1;
        }
    }
    (count > 0).then(|| sum / count as f64)
}

fn to_lonlat(east: f64, north: f64, origin: (f64, f64)) -> LonLat {
    let m_per_deg_lat = EARTH_RADIUS_M.to_radians();
    let m_per_deg_lon = EARTH_RADIUS_M.to_radians() * origin.0.to_radians().cos();
    [origin.1 + east / m_per_deg_lon, origin.0 + north / m_per_deg_lat]
}

/// The mosaic raster's 4 corners in MapLibre `image` source order: top-left,
/// top-right, bottom-right, bottom-left.
fn mosaic_corners(mosaic: &mosaic::Mosaic, origin: (f64, f64)) -> [LonLat; 4] {
    let width_m = mosaic.image.cols() as f64 * mosaic.meters_per_pixel;
    let height_m = mosaic.image.rows() as f64 * mosaic.meters_per_pixel;
    let (e0, n0) = (mosaic.origin_east, mosaic.origin_north);
    [
        to_lonlat(e0, n0, origin),
        to_lonlat(e0 + width_m, n0, origin),
        to_lonlat(e0 + width_m, n0 - height_m, origin),
        to_lonlat(e0, n0 - height_m, origin),
    ]
}
