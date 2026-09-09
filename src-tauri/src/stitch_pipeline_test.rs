//! End-to-end Quick Stitch pipeline test: two synthetic overlapping photos
//! cropped from a shared noise "world" texture (so they share real, matchable
//! content the way real overlapping drone photos would, unlike a fixture
//! reused verbatim), with metadata constructed directly rather than through
//! fake EXIF/XMP (unlike `pipeline_test.rs`) since `core::stitching::pipeline::run`
//! only needs `PhotoMeta` + a real file at `path` - it never re-reads EXIF.
#![cfg(test)]

use std::fs;
use std::path::PathBuf;

use image::{ImageBuffer, Rgb};

use crate::core::geometry;
use crate::core::stitching::pipeline;
use crate::models::photo_meta::PhotoMeta;

const EARTH_RADIUS_M: f64 = 6_378_137.0;
const ORIGIN_LAT: f64 = 10.0;
const ORIGIN_LON: f64 = 106.0;

/// A proper bit-mixing hash (murmur3-style finalizer), not a bare multiply-XOR:
/// the latter's low byte only depends on x mod 256 and y mod 256 independently,
/// which made an earlier version of this fixture accidentally near-periodic and
/// confused ORB matching with self-similar patches far from the true match.
fn hash2d(x: u32, y: u32) -> u32 {
    let mut h = x.wrapping_mul(0x9E37_79B1) ^ y.wrapping_mul(0x85EB_CA77);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297A_2D39);
    h ^= h >> 15;
    h
}

fn noise_pixel(x: u32, y: u32) -> Rgb<u8> {
    let h = hash2d(x, y);
    Rgb([(h & 0xFF) as u8, ((h >> 8) & 0xFF) as u8, ((h >> 16) & 0xFF) as u8])
}

fn to_lonlat(east_m: f64, north_m: f64) -> (f64, f64) {
    let m_per_deg_lat = EARTH_RADIUS_M.to_radians();
    let m_per_deg_lon = EARTH_RADIUS_M.to_radians() * ORIGIN_LAT.to_radians().cos();
    (ORIGIN_LAT + north_m / m_per_deg_lat, ORIGIN_LON + east_m / m_per_deg_lon)
}

fn photo_meta(lat: f64, lon: f64, path: PathBuf) -> PhotoMeta {
    let relative_altitude = 80.0;
    let focal_mm = 8.8;
    let sensor_width_mm = 13.2;
    let sensor_height_mm = 8.8;
    let yaw_deg = 0.0;

    let footprint = geometry::compute_footprint(geometry::FootprintInput {
        lat,
        lon,
        relative_altitude_m: relative_altitude,
        focal_mm,
        sensor_width_mm,
        sensor_height_mm,
        yaw_deg,
    });

    PhotoMeta {
        file_name: path.file_name().unwrap().to_string_lossy().to_string(),
        path: path.to_string_lossy().to_string(),
        lat,
        lon,
        relative_altitude: Some(relative_altitude),
        yaw_deg: Some(yaw_deg),
        focal_mm: Some(focal_mm),
        sensor_width_mm: Some(sensor_width_mm),
        sensor_height_mm: Some(sensor_height_mm),
        image_width_px: Some(1200),
        image_height_px: Some(800),
        footprint,
        capture_time: None,
        is_blurry: false,
        blur_score: None,
        warnings: Vec::new(),
    }
}

#[test]
fn full_stitch_pipeline_on_two_overlapping_synthetic_photos() {
    // At 80m altitude / 8.8mm focal / 13.2x8.8mm sensor, ground coverage is
    // 120m x 80m over a 1200x800 image: exactly 0.1 m/px on both axes.
    const MPP: f64 = 0.1;
    const PHOTO_W: u32 = 1200;
    const PHOTO_H: u32 = 800;
    const STEP_PX: u32 = 720; // 60% stride -> 40% overlap between neighbors
    const WORLD_W: u32 = PHOTO_W + STEP_PX;

    let world: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(WORLD_W, PHOTO_H, |x, y| noise_pixel(x, y));

    let dir = tempfile::tempdir().expect("tempdir");
    let mut photos = Vec::new();
    for i in 0..2u32 {
        let x_offset = i * STEP_PX;
        let crop = image::imageops::crop_imm(&world, x_offset, 0, PHOTO_W, PHOTO_H).to_image();
        let path = dir.path().join(format!("photo_{i}.jpg"));
        crop.save(&path).expect("save synthetic photo");

        // Photo center in world pixels -> local ENU meters (photo 0's center is
        // the origin) -> lat/lon.
        let center_x = x_offset as f64 + PHOTO_W as f64 / 2.0;
        let east_m = (center_x - PHOTO_W as f64 / 2.0) * MPP;
        let (lat, lon) = to_lonlat(east_m, 0.0);
        photos.push(photo_meta(lat, lon, path));
    }

    for p in &photos {
        assert!(p.footprint.is_some(), "synthetic metadata should produce a footprint");
    }

    let output_path = dir.path().join("mosaic.tif");
    let result = pipeline::run(photos, &output_path, |_| {}).expect("pipeline should succeed");

    assert_eq!(result.photos_used, 2);
    assert_eq!(result.photos_skipped, 0);
    assert_eq!(result.confident_pairs, 1, "the two overlapping photos should form one confident edge");
    assert!(output_path.exists(), "GeoTIFF should have been written");
    assert!(fs::metadata(&output_path).unwrap().len() > 0);
    assert!(PathBuf::from(&result.preview_path).exists(), "PNG preview should have been written");

    // The mosaic should roughly span the full flight extent (two 120m-wide
    // photos overlapping 40% -> combined width around 192m) rather than
    // collapsing to one photo's extent or exploding to something absurd.
    let file = std::fs::File::open(&result.geotiff_path).expect("open written geotiff");
    let mut decoder = tiff::decoder::Decoder::new(file).expect("decode geotiff");
    let (width_px, _height_px) = decoder.dimensions().expect("dimensions");
    let width_m = width_px as f64 * MPP;
    assert!(width_m > 150.0 && width_m < 250.0, "unexpected mosaic width: {width_m}m");
}
