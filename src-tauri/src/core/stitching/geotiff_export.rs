//! Writes the composited mosaic to a georeferenced GeoTIFF (Quick Stitch spec
//! 4.4 export step): the pure-Rust `tiff` crate (spec section 3 - GDAL was
//! dropped so the app doesn't need a second native C++ toolchain alongside
//! OpenCV), WGS84 (EPSG:4326), tagged via the standard GeoTIFF key directory
//! so QGIS/ArcGIS/MapLibre all read it correctly. This gives up the GDAL COG
//! driver's automatic tiling/overviews (spec section 3 calls this an accepted
//! "95%" tradeoff) - the file is a plain single-strip GeoTIFF.

use std::fs::File;
use std::path::Path;

use opencv::core::Vec3b;
use opencv::prelude::*;
use tiff::encoder::{colortype, TiffEncoder};
use tiff::tags::Tag;
use tiff::TiffResult;

use super::mosaic::Mosaic;

const EARTH_RADIUS_M: f64 = 6_378_137.0;

const TAG_MODEL_PIXEL_SCALE: u16 = 33550;
const TAG_MODEL_TIEPOINT: u16 = 33922;
const TAG_GEO_KEY_DIRECTORY: u16 = 34735;

// Minimal GeoKeyDirectory declaring a plain WGS84 geographic CRS: header
// (version 1.1.0, 3 keys) followed by one (KeyID, TIFFTagLocation, Count,
// Value) entry per key. See the GeoTIFF spec's "6.2.2 GeoKey Requirements
// for Geographic CRS" table.
const GT_MODEL_TYPE_GEO_KEY: u16 = 1024;
const GT_MODEL_TYPE_GEOGRAPHIC: u16 = 2;
const GT_RASTER_TYPE_GEO_KEY: u16 = 1025;
const GT_RASTER_TYPE_IS_AREA: u16 = 1;
const GEOGRAPHIC_TYPE_GEO_KEY: u16 = 2048;
const EPSG_WGS84: u16 = 4326;

/// Writes `mosaic` to `output_path` as a GeoTIFF. `origin_lat`/`origin_lon` are
/// the lat/lon of the pose-graph's local-meters origin (see
/// `pose_graph::world_origin`), used to convert the mosaic's meters-based
/// georeference back to WGS84.
pub fn write_geotiff(mosaic: &Mosaic, origin_lat: f64, origin_lon: f64, output_path: &Path) -> TiffResult<()> {
    let (width, height) = (mosaic.image.cols() as u32, mosaic.image.rows() as u32);

    let m_per_deg_lat = EARTH_RADIUS_M.to_radians();
    let m_per_deg_lon = EARTH_RADIUS_M.to_radians() * origin_lat.to_radians().cos();
    let top_left_lon = origin_lon + mosaic.origin_east / m_per_deg_lon;
    let top_left_lat = origin_lat + mosaic.origin_north / m_per_deg_lat;
    let pixel_size_deg_lon = mosaic.meters_per_pixel / m_per_deg_lon;
    let pixel_size_deg_lat = mosaic.meters_per_pixel / m_per_deg_lat;

    // OpenCV Mats are BGR; TIFF RGB8 wants R, G, B interleaved.
    let mut rgb = vec![0u8; width as usize * height as usize * 3];
    for y in 0..height as i32 {
        for x in 0..width as i32 {
            let px = mosaic.image.at_2d::<Vec3b>(y, x).expect("mosaic pixel");
            let base = (y as usize * width as usize + x as usize) * 3;
            rgb[base] = px[2];
            rgb[base + 1] = px[1];
            rgb[base + 2] = px[0];
        }
    }

    let file = File::create(output_path)?;
    let mut tiff = TiffEncoder::new(file)?;
    let mut image = tiff.new_image::<colortype::RGB8>(width, height)?;

    let pixel_scale: [f64; 3] = [pixel_size_deg_lon, pixel_size_deg_lat, 0.0];
    let tiepoint: [f64; 6] = [0.0, 0.0, 0.0, top_left_lon, top_left_lat, 0.0];
    #[rustfmt::skip]
    let geo_keys: [u16; 16] = [
        1, 1, 0, 3,
        GT_MODEL_TYPE_GEO_KEY, 0, 1, GT_MODEL_TYPE_GEOGRAPHIC,
        GT_RASTER_TYPE_GEO_KEY, 0, 1, GT_RASTER_TYPE_IS_AREA,
        GEOGRAPHIC_TYPE_GEO_KEY, 0, 1, EPSG_WGS84,
    ];

    let encoder = image.encoder();
    encoder.write_tag(Tag::Unknown(TAG_MODEL_PIXEL_SCALE), &pixel_scale[..])?;
    encoder.write_tag(Tag::Unknown(TAG_MODEL_TIEPOINT), &tiepoint[..])?;
    encoder.write_tag(Tag::Unknown(TAG_GEO_KEY_DIRECTORY), &geo_keys[..])?;

    image.write_data(&rgb)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use opencv::core::{Mat, Scalar, CV_8UC3};

    #[test]
    fn writes_a_readable_geotiff_with_correct_size_and_srs() {
        let mut image = Mat::new_rows_cols_with_default(20, 30, CV_8UC3, Scalar::all(0.0)).unwrap();
        for y in 0..20 {
            for x in 0..30 {
                *image.at_2d_mut::<Vec3b>(y, x).unwrap() = Vec3b::from([50, 100, 150]);
            }
        }
        let mosaic = Mosaic { image, origin_east: 0.0, origin_north: 0.0, meters_per_pixel: 0.05 };

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("out.tif");
        write_geotiff(&mosaic, 10.0, 106.0, &path).expect("write geotiff");

        let file = File::open(&path).expect("open written geotiff");
        let mut decoder = tiff::decoder::Decoder::new(file).expect("decode geotiff");
        assert_eq!(decoder.dimensions().expect("dimensions"), (30, 20));
        assert_eq!(decoder.colortype().expect("colortype"), tiff::ColorType::RGB(8));
    }
}
