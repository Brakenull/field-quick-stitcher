//! One-off manual benchmark against a real drone photo set, not part of the
//! normal test suite (`#[ignore]`d). Run with:
//!   cargo test --release real_stitch_benchmark -- --ignored --nocapture
#![cfg(test)]

use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::core::stitching::pipeline;
use crate::core::{blur_detector, geometry, metadata_parser};
use crate::models::photo_meta::PhotoMeta;
use crate::utils::thread_pool::par_map_with_progress;

const REAL_DATA_DIR: &str = r"C:\Users\brake\Local\Cowork\search-test_images\Result\Jablunkov_Pass_Fortifications_CZ";

fn parse_one(path: PathBuf) -> PhotoMeta {
    let mut meta = metadata_parser::parse_photo(&path);
    let blur = blur_detector::analyze(&path);
    meta.is_blurry = blur.is_blurry;
    meta.blur_score = blur.score;

    if let (Some(rel_alt), Some(yaw), Some(focal), Some(sw), Some(sh)) =
        (meta.relative_altitude, meta.yaw_deg, meta.focal_mm, meta.sensor_width_mm, meta.sensor_height_mm)
    {
        meta.footprint = geometry::compute_footprint(geometry::FootprintInput {
            lat: meta.lat,
            lon: meta.lon,
            relative_altitude_m: rel_alt,
            focal_mm: focal,
            sensor_width_mm: sw,
            sensor_height_mm: sh,
            yaw_deg: yaw,
        });
    }
    meta
}

#[test]
#[ignore]
fn real_stitch_benchmark() {
    let dir = Path::new(REAL_DATA_DIR);
    assert!(dir.is_dir(), "test dataset not found at {REAL_DATA_DIR}");

    let paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| matches!(p.extension().and_then(|e| e.to_str()).map(|s| s.to_lowercase()).as_deref(), Some("jpg") | Some("jpeg")))
        .collect();
    println!("=== Found {} JPEGs ===", paths.len());

    let parse_start = Instant::now();
    let photos: Vec<PhotoMeta> = par_map_with_progress(paths, parse_one, |pct| println!("parsing: {pct}%"));
    println!("=== Parsed metadata for {} photos in {:?} ===", photos.len(), parse_start.elapsed());

    for p in photos.iter().take(3) {
        println!(
            "sample: {} lat={:.6} lon={:.6} alt={:?} yaw={:?} focal={:?} sensor=({:?},{:?}) footprint={} blurry={} warnings={:?}",
            p.file_name, p.lat, p.lon, p.relative_altitude, p.yaw_deg, p.focal_mm, p.sensor_width_mm, p.sensor_height_mm,
            p.footprint.is_some(), p.is_blurry, p.warnings
        );
    }

    let with_footprint: Vec<PhotoMeta> = photos.into_iter().filter(|p| p.footprint.is_some()).collect();
    println!("=== {} photos have a computable footprint ===", with_footprint.len());
    assert!(with_footprint.len() >= 2, "not enough photos with full metadata to stitch");

    let output_path = std::env::temp_dir().join("jablunkov_stitch_bench.tif");
    println!("=== Starting quick_stitch pipeline, output -> {} ===", output_path.display());

    let stitch_start = Instant::now();
    let result = pipeline::run(with_footprint, &output_path, |progress| println!("{:?}: {}%", progress.stage, progress.percent));
    let elapsed = stitch_start.elapsed();

    match result {
        Ok(r) => {
            println!("=== SUCCESS in {elapsed:?} ===");
            println!(
                "photos_used={} photos_skipped={} confident_pairs={} reported_duration_ms={} warnings={:?}",
                r.photos_used, r.photos_skipped, r.confident_pairs, r.duration_ms, r.warnings
            );
            println!("geotiff: {}", r.geotiff_path);
            println!("preview: {}", r.preview_path);
        }
        Err(e) => {
            println!("=== FAILED after {elapsed:?}: {e} ===");
            panic!("pipeline failed: {e}");
        }
    }
}
