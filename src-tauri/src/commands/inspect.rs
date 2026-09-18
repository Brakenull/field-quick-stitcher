use std::path::{Path, PathBuf};
use std::time::Instant;

use tauri::{AppHandle, Emitter, State};
use walkdir::WalkDir;

use crate::core::{blur_detector, geometry, metadata_parser, overlap_engine};
use crate::models::inspection_result::{BlurAlert, GeoFeature, GeoGeometry, InspectionResult, Metrics};
use crate::models::photo_meta::PhotoMeta;
use crate::utils::thread_pool::par_map_with_progress;
use crate::AppState;

/// Spatial grid cell size for the overlap heatmap, per the MVP spec (2-5m).
const CELL_SIZE_M: f64 = 3.0;

fn is_jpeg(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_lowercase())
            .as_deref(),
        Some("jpg") | Some("jpeg")
    )
}

fn parse_one(path: PathBuf) -> PhotoMeta {
    let mut meta = metadata_parser::parse_photo(&path);
    let blur = blur_detector::analyze(&path);
    meta.is_blurry = blur.is_blurry;
    meta.blur_score = blur.score;

    if let (Some(lat), Some(lon), Some(relative_altitude_m), Some(yaw_deg), Some(focal_mm), Some(sensor_width_mm), Some(sensor_height_mm)) = (
        meta.lat,
        meta.lon,
        meta.relative_altitude,
        meta.yaw_deg,
        meta.focal_mm,
        meta.sensor_width_mm,
        meta.sensor_height_mm,
    ) {
        meta.footprint = geometry::compute_footprint(geometry::FootprintInput {
            lat,
            lon,
            relative_altitude_m,
            focal_mm,
            sensor_width_mm,
            sensor_height_mm,
            yaw_deg,
        });
    }
    meta
}

#[tauri::command]
pub async fn inspect_directory(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
) -> Result<InspectionResult, String> {
    let start = Instant::now();
    let dir = PathBuf::from(&path);
    if !dir.is_dir() {
        return Err(format!("Not a directory: {path}"));
    }

    let files: Vec<PathBuf> = WalkDir::new(&dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file() && is_jpeg(e.path()))
        .map(|e| e.path().to_path_buf())
        .collect();

    if files.is_empty() {
        return Err("No JPEG photos found in this folder.".to_string());
    }

    let progress_app = app.clone();
    let mut photos: Vec<PhotoMeta> = tauri::async_runtime::spawn_blocking(move || {
        par_map_with_progress(files, parse_one, move |percent| {
            let _ = progress_app.emit("scan_progress", percent);
        })
    })
    .await
    .map_err(|e| e.to_string())?;

    // Photos lacking GPS are kept (not dropped) so a mid-flight GPS dropout is
    // visible in `photos`/`Metrics::photos_with_gps` rather than silently
    // vanishing - but a scan where *nothing* has GPS still can't produce a
    // useful flight path/heatmap, so that case is still an error.
    if !photos.iter().any(|p| p.lat.is_some() && p.lon.is_some()) {
        return Err("None of the photos had usable GPS metadata.".to_string());
    }
    photos.sort_by(|a, b| a.sort_key.cmp(&b.sort_key).then(a.file_name.cmp(&b.file_name)));

    let footprints: Vec<_> = photos.iter().filter_map(|p| p.footprint.clone()).collect();
    let overlap = overlap_engine::analyze(&footprints, CELL_SIZE_M);

    let flight_path = build_flight_path(&photos);
    let blur_alerts: Vec<BlurAlert> = photos
        .iter()
        .filter(|p| p.is_blurry)
        .filter_map(|p| {
            Some(BlurAlert {
                file_name: p.file_name.clone(),
                lat: p.lat?,
                lon: p.lon?,
                blur_score: p.blur_score.unwrap_or(0.0),
            })
        })
        .collect();

    let altitudes: Vec<f64> = photos.iter().filter_map(|p| p.relative_altitude).collect();
    let avg_altitude_m = if altitudes.is_empty() {
        None
    } else {
        Some(altitudes.iter().sum::<f64>() / altitudes.len() as f64)
    };

    let metrics = Metrics {
        total_photos: photos.len(),
        photos_with_gps: photos.iter().filter(|p| p.lat.is_some() && p.lon.is_some()).count(),
        // Counts every blurry photo, including any without GPS - `blur_alerts`
        // (map pins) can only plot the ones with coordinates, but the count
        // shouldn't understate how many blurry shots the scan actually found.
        blurry_count: photos.iter().filter(|p| p.is_blurry).count(),
        gap_count: overlap.gaps.len(),
        avg_altitude_m,
        coverage_area_m2: overlap.coverage_area_m2,
        scan_duration_ms: start.elapsed().as_millis(),
    };

    let result = InspectionResult {
        photos,
        flight_path,
        heatmap: overlap.heatmap,
        gaps: overlap.gaps,
        blur_alerts,
        metrics,
    };

    *state.last_inspection.lock().map_err(|e| e.to_string())? = Some(result.clone());
    Ok(result)
}

/// Builds the flight-path line and per-photo point layer from whichever
/// photos actually have coordinates - a photo with a GPS dropout (`lat`/`lon`
/// both `None`) still exists in `photos` (see `PhotoMeta::lat`) but simply
/// can't be placed on the map, so it's skipped here rather than plotted at a
/// placeholder position.
fn build_flight_path(photos: &[PhotoMeta]) -> Vec<GeoFeature> {
    let located: Vec<&PhotoMeta> = photos.iter().filter(|p| p.lat.is_some() && p.lon.is_some()).collect();
    if located.is_empty() {
        return Vec::new();
    }
    let mut features = Vec::with_capacity(located.len() + 1);
    let coords: Vec<_> = located.iter().map(|p| [p.lon.unwrap(), p.lat.unwrap()]).collect();
    features.push(GeoFeature {
        feature_type: "Feature",
        geometry: GeoGeometry::LineString(coords),
        properties: serde_json::json!({ "kind": "flightPath" }),
    });
    for p in located {
        features.push(GeoFeature {
            feature_type: "Feature",
            geometry: GeoGeometry::Point([p.lon.unwrap(), p.lat.unwrap()]),
            properties: serde_json::json!({
                "kind": "photoPoint",
                "fileName": p.file_name,
                "isBlurry": p.is_blurry,
            }),
        });
    }
    features
}
