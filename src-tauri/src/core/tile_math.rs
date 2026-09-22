//! Slippy-map (Web Mercator) tile enumeration for a geographic bounding box:
//! pure math, no I/O, used by `commands::offline_basemap` to turn a user's
//! lon/lat bbox into the exact list of z/x/y tiles to fetch.

use crate::models::offline_basemap::BasemapBbox;

/// Web Mercator's valid latitude range - beyond this the projection diverges
/// to infinity, so any bbox latitude is clamped into this range before use.
const MAX_MERCATOR_LAT: f64 = 85.0511;

/// Hard cap on `max_zoom` accepted by `tiles_for_bbox` - a little headroom
/// over the `--maxzoom=14` this project's own regional-extract examples use
/// (see `public/offline_tiles/README.md`), while still rejecting an
/// accidental request for true street-level-plus detail nobody asked for.
pub const MAX_ZOOM_LEVEL: u8 = 15;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileXY {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

fn validate(bbox: &BasemapBbox, max_zoom: u8) -> Result<(), String> {
    if !bbox.min_lon.is_finite() || !bbox.max_lon.is_finite() || !bbox.min_lat.is_finite() || !bbox.max_lat.is_finite() {
        return Err("Bounding box coordinates must be finite numbers.".to_string());
    }
    if bbox.min_lon < -180.0 || bbox.max_lon > 180.0 {
        return Err("Longitude must be within [-180, 180].".to_string());
    }
    if bbox.min_lat < -90.0 || bbox.max_lat > 90.0 {
        return Err("Latitude must be within [-90, 90].".to_string());
    }
    if bbox.min_lon >= bbox.max_lon {
        return Err("Bounding box min longitude must be less than max longitude.".to_string());
    }
    if bbox.min_lat >= bbox.max_lat {
        return Err("Bounding box min latitude must be less than max latitude.".to_string());
    }
    if max_zoom > MAX_ZOOM_LEVEL {
        return Err(format!("Max zoom must be {MAX_ZOOM_LEVEL} or lower."));
    }
    Ok(())
}

fn lon_to_tile_x(lon: f64, z: u8) -> i64 {
    let n = 2f64.powi(z as i32);
    ((lon + 180.0) / 360.0 * n).floor() as i64
}

fn lat_to_tile_y(lat: f64, z: u8) -> i64 {
    let lat = lat.clamp(-MAX_MERCATOR_LAT, MAX_MERCATOR_LAT);
    let n = 2f64.powi(z as i32);
    let lat_rad = lat.to_radians();
    ((1.0 - (lat_rad.tan() + 1.0 / lat_rad.cos()).ln() / std::f64::consts::PI) / 2.0 * n).floor() as i64
}

/// Enumerates every slippy-map tile from z=0 through `max_zoom` whose extent
/// intersects `bbox`. z=0 always yields exactly the single world tile;
/// coverage naturally narrows to just the bbox's own tiles as z increases.
pub fn tiles_for_bbox(bbox: &BasemapBbox, max_zoom: u8) -> Result<Vec<TileXY>, String> {
    validate(bbox, max_zoom)?;

    let mut tiles = Vec::new();
    for z in 0..=max_zoom {
        let max_index = (1i64 << z) - 1;
        let x_min = lon_to_tile_x(bbox.min_lon, z).clamp(0, max_index);
        let x_max = lon_to_tile_x(bbox.max_lon, z).clamp(0, max_index);
        // Latitude grows northward but tile y grows southward, so max_lat -> smaller y.
        let y_min = lat_to_tile_y(bbox.max_lat, z).clamp(0, max_index);
        let y_max = lat_to_tile_y(bbox.min_lat, z).clamp(0, max_index);

        for x in x_min..=x_max {
            for y in y_min..=y_max {
                tiles.push(TileXY { z, x: x as u32, y: y as u32 });
            }
        }
    }
    Ok(tiles)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jablunkov_bbox() -> BasemapBbox {
        BasemapBbox { min_lon: 18.6, min_lat: 49.4, max_lon: 18.9, max_lat: 49.6 }
    }

    #[test]
    fn z0_always_yields_exactly_one_tile() {
        let tiles = tiles_for_bbox(&jablunkov_bbox(), 3).unwrap();
        let z0_count = tiles.iter().filter(|t| t.z == 0).count();
        assert_eq!(z0_count, 1);
    }

    #[test]
    fn tile_count_grows_monotonically_and_matches_known_extract() {
        let tiles = tiles_for_bbox(&jablunkov_bbox(), 14).unwrap();
        let count_at = |z: u8| tiles.iter().filter(|t| t.z == z).count();
        // Non-decreasing per zoom - a ~25km bbox stays within a single tile
        // at low zoom (tile width only drops below ~25km around z=11), then
        // fans out; it should never shrink as zoom increases.
        for z in 0..14 {
            assert!(count_at(z + 1) >= count_at(z), "zoom {z}->{}: {} -> {}", z + 1, count_at(z), count_at(z + 1));
        }
        assert_eq!(count_at(0), 1);
        assert!(count_at(11) > 1, "expected multiple tiles once tile width drops below the bbox span");
        // Sanity range around the real `go-pmtiles extract --bbox=18.6,49.4,18.9,49.6
        // --maxzoom=14` run against this exact bbox this session (327 tiles) -
        // not an exact match requirement (different tools may clip edge tiles
        // slightly differently), just a regression guard against gross drift.
        let total: usize = tiles.len();
        assert!((200..500).contains(&total), "total tile count {total} outside expected range");
    }

    #[test]
    fn degenerate_min_equals_max_is_rejected() {
        let bbox = BasemapBbox { min_lon: 18.75, min_lat: 49.5, max_lon: 18.75, max_lat: 49.5 };
        assert!(tiles_for_bbox(&bbox, 5).is_err());
    }

    #[test]
    fn out_of_range_longitude_is_rejected() {
        let bbox = BasemapBbox { min_lon: -200.0, min_lat: 49.4, max_lon: 18.9, max_lat: 49.6 };
        assert!(tiles_for_bbox(&bbox, 5).is_err());
    }

    #[test]
    fn inverted_bbox_is_rejected() {
        let bbox = BasemapBbox { min_lon: 18.9, min_lat: 49.4, max_lon: 18.6, max_lat: 49.6 };
        assert!(tiles_for_bbox(&bbox, 5).is_err());
    }

    #[test]
    fn max_zoom_above_cap_is_rejected() {
        assert!(tiles_for_bbox(&jablunkov_bbox(), MAX_ZOOM_LEVEL + 1).is_err());
    }
}
