//! End-to-end Quick Stitch pipeline test against two real, naturally
//! overlapping drone photos (metadata parsed from their real EXIF/XMP via
//! `metadata_parser`, the same way `real_data_bench.rs` does) - unlike
//! `pipeline_test.rs`'s hand-built EXIF fixtures for Phase 1.
//!
//! This used to run against a synthetic fixture (two crops sharing pixel-
//! identical content, or even hand-drawn shapes), which worked fine for the
//! ORB path but not for `onnx_matcher.rs`'s fused SuperPoint+LightGlue model:
//! cropping either photo to an arbitrary sub-region - even the *same* region
//! of both, guaranteeing pixel-perfect correspondence - repeatedly produced
//! zero or near-zero matches, while the exact same two full-resolution files
//! reliably produce 550+ (see `onnx_matcher::tests::matches_two_real_overlapping_photos`).
//! Diagnosed as two compounding issues, not a pipeline bug: (1) sequential
//! flight photos overlap at their *edges*, not their centers, so an arbitrary
//! same-sized crop of each has no guarantee of covering the same ground at
//! all; (2) the specific region sampled in earlier attempts happened to be
//! low-texture farmland, which even a perfect crop of would give a real
//! matcher little confident structure to find. Using the untouched real
//! files sidesteps both issues entirely, at the cost of needing this dataset
//! on disk (see `REAL_DATA_DIR`, `#[ignore]`d for the same reason as
//! `real_data_bench::real_stitch_benchmark`).
#![cfg(test)]

use std::path::{Path, PathBuf};

use crate::core::stitching::pipeline;
use crate::core::{blur_detector, geometry, metadata_parser};
use crate::models::photo_meta::PhotoMeta;

const REAL_DATA_DIR: &str = r"C:\Users\brake\Local\Cowork\search-test_images\Result\Jablunkov_Pass_Fortifications_CZ";

fn parse_photo(path: &Path) -> PhotoMeta {
    let mut meta = metadata_parser::parse_photo(path);
    let blur = blur_detector::analyze(path);
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

/// Needs `REAL_DATA_DIR` on disk (not available in CI/most dev machines) -
/// run explicitly with `-- --ignored`, same as `real_data_bench::real_stitch_benchmark`.
#[test]
#[ignore]
fn full_stitch_pipeline_on_two_real_overlapping_photos() {
    let dir = Path::new(REAL_DATA_DIR);
    assert!(dir.is_dir(), "test dataset not found at {REAL_DATA_DIR}");

    let photos: Vec<PhotoMeta> =
        ["dji_0001.jpg", "dji_0002.jpg"].iter().map(|name| parse_photo(&dir.join(name))).collect();
    for p in &photos {
        assert!(p.footprint.is_some(), "{} should have a computable footprint", p.file_name);
    }

    let out_dir = tempfile::tempdir().expect("tempdir");
    let output_path = out_dir.path().join("mosaic.tif");
    let onnx_model_path = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/resources/superpoint-ort.onnx"));
    let result = pipeline::run(photos, &output_path, onnx_model_path, |_| {}).expect("pipeline should succeed");

    assert_eq!(result.photos_used, 2);
    assert_eq!(result.photos_skipped, 0);
    assert_eq!(result.confident_pairs, 1, "the two overlapping photos should form one confident edge");
    assert!(output_path.exists(), "GeoTIFF should have been written");
    assert!(std::fs::metadata(&output_path).unwrap().len() > 0);
    assert!(PathBuf::from(&result.preview_path).exists(), "PNG preview should have been written");
}
