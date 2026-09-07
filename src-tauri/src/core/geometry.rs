//! Footprint Projection: projects a photo's ground coverage rectangle from
//! (relative altitude, focal length, sensor size, yaw) onto WGS84 lat/lon.

use crate::models::photo_meta::LonLat;

/// WGS84-ish mean earth radius (meters), good enough for local flat-earth offsets
/// over a single flight area (a few km across).
const EARTH_RADIUS_M: f64 = 6_378_137.0;

#[derive(Debug, Clone, Copy)]
pub struct FootprintInput {
    pub lat: f64,
    pub lon: f64,
    /// Height above ground level, meters.
    pub relative_altitude_m: f64,
    pub focal_mm: f64,
    pub sensor_width_mm: f64,
    pub sensor_height_mm: f64,
    /// Compass heading, degrees, 0 = North, clockwise positive.
    pub yaw_deg: f64,
}

/// Ground coverage width/height in meters, from altitude/focal/sensor.
pub fn ground_coverage_m(relative_altitude_m: f64, focal_mm: f64, sensor_w_mm: f64, sensor_h_mm: f64) -> (f64, f64) {
    let w = relative_altitude_m * (sensor_w_mm / focal_mm);
    let h = relative_altitude_m * (sensor_h_mm / focal_mm);
    (w, h)
}

/// Offsets a lat/lon point by (east_m, north_m) meters, returning [lon, lat].
fn offset_latlon(lat: f64, lon: f64, east_m: f64, north_m: f64) -> LonLat {
    let lat_rad = lat.to_radians();
    let d_lat_deg = (north_m / EARTH_RADIUS_M).to_degrees();
    let d_lon_deg = (east_m / (EARTH_RADIUS_M * lat_rad.cos())).to_degrees();
    [lon + d_lon_deg, lat + d_lat_deg]
}

/// Computes the 4-cornered ground footprint polygon (closed ring, 5 points) for a photo.
/// Returns `None` if the inputs can't produce a sane footprint (non-finite, non-positive
/// altitude/focal length).
pub fn compute_footprint(input: FootprintInput) -> Option<Vec<LonLat>> {
    if input.relative_altitude_m <= 0.0 || input.focal_mm <= 0.0 {
        return None;
    }
    if !input.lat.is_finite() || !input.lon.is_finite() || !input.yaw_deg.is_finite() {
        return None;
    }

    let (w, h) = ground_coverage_m(
        input.relative_altitude_m,
        input.focal_mm,
        input.sensor_width_mm,
        input.sensor_height_mm,
    );
    if !w.is_finite() || !h.is_finite() || w <= 0.0 || h <= 0.0 {
        return None;
    }

    // Local ENU corners before rotation: x = east offset, y = north offset.
    // Order: bottom-left, bottom-right, top-right, top-left (image-space winding).
    let local_corners = [
        (-w / 2.0, -h / 2.0),
        (w / 2.0, -h / 2.0),
        (w / 2.0, h / 2.0),
        (-w / 2.0, h / 2.0),
    ];

    let yaw_rad = input.yaw_deg.to_radians();
    let (sin_y, cos_y) = yaw_rad.sin_cos();

    let mut ring: Vec<LonLat> = local_corners
        .iter()
        .map(|&(x, y)| {
            // Clockwise rotation by compass bearing yaw, in an (east, north) frame.
            let east = x * cos_y + y * sin_y;
            let north = -x * sin_y + y * cos_y;
            offset_latlon(input.lat, input.lon, east, north)
        })
        .collect();

    // Close the ring for GeoJSON Polygon validity.
    ring.push(ring[0]);
    Some(ring)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ground_coverage_scales_with_altitude() {
        let (w, h) = ground_coverage_m(100.0, 10.0, 13.2, 8.8);
        assert!((w - 132.0).abs() < 1e-6);
        assert!((h - 88.0).abs() < 1e-6);
    }

    #[test]
    fn footprint_at_yaw_zero_is_axis_aligned() {
        let input = FootprintInput {
            lat: 10.0,
            lon: 106.0,
            relative_altitude_m: 100.0,
            focal_mm: 10.0,
            sensor_width_mm: 13.2,
            sensor_height_mm: 8.8,
            yaw_deg: 0.0,
        };
        let ring = compute_footprint(input).expect("footprint");
        assert_eq!(ring.len(), 5);
        assert_eq!(ring[0], ring[4], "ring must be closed");

        // At yaw 0: corner 0 is south-west (min lon, min lat), corner 2 is north-east.
        assert!(ring[0][0] < input.lon && ring[0][1] < input.lat);
        assert!(ring[2][0] > input.lon && ring[2][1] > input.lat);

        // Width/height in degrees should roughly match ground_coverage_m converted back.
        let (w_m, h_m) = ground_coverage_m(100.0, 10.0, 13.2, 8.8);
        let lat_span_m = (ring[2][1] - ring[0][1]).to_radians() * EARTH_RADIUS_M;
        assert!((lat_span_m - h_m).abs() < 0.1);
        let lon_span_m =
            (ring[2][0] - ring[0][0]).to_radians() * EARTH_RADIUS_M * input.lat.to_radians().cos();
        assert!((lon_span_m - w_m).abs() < 0.1);
    }

    #[test]
    fn footprint_at_yaw_90_rotates_north_edge_to_east() {
        // Use a near-zero-width sensor so the top edge (local y = +h/2) is
        // essentially a pure-north point before rotation. With yaw=90, that point
        // should end up essentially due-east instead.
        let input = FootprintInput {
            lat: 0.0,
            lon: 0.0,
            relative_altitude_m: 100.0,
            focal_mm: 10.0,
            sensor_width_mm: 0.001,
            sensor_height_mm: 10.0,
            yaw_deg: 90.0,
        };
        let ring = compute_footprint(input).expect("footprint");
        // corner 2 (top-right in image space, local (w/2, h/2)) is ~due-north pre-rotation.
        assert!(ring[2][1].abs() < 1e-6, "should have ~0 lat offset after 90deg rotation");
        assert!(ring[2][0] > 0.0);
    }

    #[test]
    fn invalid_inputs_return_none() {
        let mut input = FootprintInput {
            lat: 10.0,
            lon: 106.0,
            relative_altitude_m: 0.0,
            focal_mm: 10.0,
            sensor_width_mm: 13.2,
            sensor_height_mm: 8.8,
            yaw_deg: 0.0,
        };
        assert!(compute_footprint(input).is_none());
        input.relative_altitude_m = 100.0;
        input.focal_mm = 0.0;
        assert!(compute_footprint(input).is_none());
    }
}
