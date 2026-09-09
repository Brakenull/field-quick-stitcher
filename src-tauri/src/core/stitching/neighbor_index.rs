//! GPS-guided neighbor lookup: restricts feature matching to photo pairs whose
//! ground footprints are close enough to plausibly overlap, instead of the
//! O(N^2) all-pairs comparison a naive stitcher would do. Two-stage filter
//! matching the spec's two descriptions of this step: a cheap R-tree distance
//! radius first (section 2b: `R ≈ W_ground`, keeps this sub-O(N^2)), then an
//! exact footprint-polygon overlap-area check (section 6.1.b: >30% overlap)
//! on just the surviving candidates.

use std::collections::HashSet;

use rstar::{RTree, RTreeObject, AABB};

use crate::core::geometry;
use crate::models::photo_meta::{LonLat, PhotoMeta};

const EARTH_RADIUS_M: f64 = 6_378_137.0;
/// Minimum footprint-overlap fraction (of the smaller photo's area) to treat
/// two photos as neighbors worth matching, per spec section 6.1.b.
const MIN_OVERLAP_FRACTION: f64 = 0.3;

struct IndexedCenter {
    idx: usize,
    lon: f64,
    lat: f64,
}

impl RTreeObject for IndexedCenter {
    type Envelope = AABB<[f64; 2]>;
    fn envelope(&self) -> Self::Envelope {
        AABB::from_point([self.lon, self.lat])
    }
}

/// A reasonable neighbor-search radius in meters: the largest ground-footprint
/// diagonal among the given photos, so guided matching won't miss genuinely
/// overlapping neighbors even for the widest-altitude shot in the set. Only
/// considers photos with enough metadata to have a footprint; returns 0.0 if
/// none do (callers should treat that as "no reliable neighbors").
pub fn typical_radius_m(photos: &[PhotoMeta]) -> f64 {
    photos
        .iter()
        .filter_map(|p| {
            let (w, h) = geometry::ground_coverage_m(
                p.relative_altitude?,
                p.focal_mm?,
                p.sensor_width_mm?,
                p.sensor_height_mm?,
            );
            Some((w * w + h * h).sqrt())
        })
        .fold(0.0_f64, f64::max)
}

/// Unordered pairs of photo indices (i < j) that are both within `radius_m` of
/// each other AND (when both have a computed footprint) overlap by more than
/// [`MIN_OVERLAP_FRACTION`] of the smaller footprint's area. `photos` should be
/// indexed the same way the caller will use the returned indices (e.g. the
/// same slice passed to feature matching).
pub fn find_neighbor_pairs(photos: &[PhotoMeta], radius_m: f64) -> Vec<(usize, usize)> {
    if photos.len() < 2 || radius_m <= 0.0 {
        return Vec::new();
    }

    let mid_lat = photos.iter().map(|p| p.lat).sum::<f64>() / photos.len() as f64;
    let mid_lat_rad = mid_lat.to_radians();
    let m_per_deg_lat = EARTH_RADIUS_M.to_radians();
    let m_per_deg_lon = EARTH_RADIUS_M.to_radians() * mid_lat_rad.cos();

    // Conservative (smaller) meters-per-degree so the envelope query never
    // under-searches; the exact-distance check below filters out any slack.
    let radius_deg = radius_m / m_per_deg_lon.min(m_per_deg_lat);

    let rtree = RTree::bulk_load(
        photos
            .iter()
            .enumerate()
            .map(|(idx, p)| IndexedCenter { idx, lon: p.lon, lat: p.lat })
            .collect(),
    );

    let mut pairs = HashSet::new();
    for center in rtree.iter() {
        let query = AABB::from_corners(
            [center.lon - radius_deg, center.lat - radius_deg],
            [center.lon + radius_deg, center.lat + radius_deg],
        );
        for other in rtree.locate_in_envelope_intersecting(&query) {
            if other.idx == center.idx {
                continue;
            }
            let dx = (other.lon - center.lon) * m_per_deg_lon;
            let dy = (other.lat - center.lat) * m_per_deg_lat;
            if (dx * dx + dy * dy).sqrt() <= radius_m {
                let pair = if center.idx < other.idx { (center.idx, other.idx) } else { (other.idx, center.idx) };
                pairs.insert(pair);
            }
        }
    }

    let mut result: Vec<_> = pairs
        .into_iter()
        .filter(|&(i, j)| passes_overlap_check(&photos[i], &photos[j]))
        .collect();
    result.sort_unstable();
    result
}

/// `true` unless both photos have a footprint AND that footprint's overlap
/// fraction falls below the threshold - i.e. this only ever *rejects* pairs,
/// never adds ones the radius check missed, and gracefully degrades to
/// radius-only when footprint data isn't available.
fn passes_overlap_check(a: &PhotoMeta, b: &PhotoMeta) -> bool {
    match (&a.footprint, &b.footprint) {
        (Some(fa), Some(fb)) => overlap_fraction(fa, fb, a.lat, a.lon) > MIN_OVERLAP_FRACTION,
        _ => true,
    }
}

/// Fraction of the smaller footprint's area that the two footprints overlap,
/// via exact convex-polygon clipping (footprints are rotated rectangles, so
/// always convex) in a local meters projection centered near `origin_lat`.
fn overlap_fraction(footprint_a: &[LonLat], footprint_b: &[LonLat], origin_lat: f64, origin_lon: f64) -> f64 {
    let m_per_deg_lat = EARTH_RADIUS_M.to_radians();
    let m_per_deg_lon = EARTH_RADIUS_M.to_radians() * origin_lat.to_radians().cos();
    let to_local = |ring: &[LonLat]| -> Vec<(f64, f64)> {
        // Footprint rings are closed (last point repeats the first); drop it
        // so the clipper works with the 4 unique vertices.
        ring.iter()
            .take(ring.len().saturating_sub(1).max(1))
            .map(|p| ((p[0] - origin_lon) * m_per_deg_lon, (p[1] - origin_lat) * m_per_deg_lat))
            .collect()
    };

    let a = to_local(footprint_a);
    let b = to_local(footprint_b);
    let area_a = polygon_area(&a);
    let area_b = polygon_area(&b);
    if area_a <= 0.0 || area_b <= 0.0 {
        return 0.0;
    }

    let intersection = clip_polygon(&a, &b);
    polygon_area(&intersection) / area_a.min(area_b)
}

fn polygon_area(points: &[(f64, f64)]) -> f64 {
    if points.len() < 3 {
        return 0.0;
    }
    let mut sum = 0.0;
    for i in 0..points.len() {
        let (x1, y1) = points[i];
        let (x2, y2) = points[(i + 1) % points.len()];
        sum += x1 * y2 - x2 * y1;
    }
    (sum / 2.0).abs()
}

/// Sutherland-Hodgman polygon clipping: clips `subject` against the convex
/// polygon `clip`. Both must be wound consistently (our footprints are always
/// CCW - `geometry::compute_footprint`'s corner order traces bottom-left ->
/// bottom-right -> top-right -> top-left, counter-clockwise in east/north).
fn clip_polygon(subject: &[(f64, f64)], clip: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut output = subject.to_vec();
    for i in 0..clip.len() {
        if output.is_empty() {
            break;
        }
        let edge_a = clip[i];
        let edge_b = clip[(i + 1) % clip.len()];
        let input = std::mem::take(&mut output);
        for j in 0..input.len() {
            let cur = input[j];
            let prev = input[(j + input.len() - 1) % input.len()];
            let cur_inside = is_left_of(edge_a, edge_b, cur);
            let prev_inside = is_left_of(edge_a, edge_b, prev);
            if cur_inside {
                if !prev_inside {
                    output.push(line_intersection(prev, cur, edge_a, edge_b));
                }
                output.push(cur);
            } else if prev_inside {
                output.push(line_intersection(prev, cur, edge_a, edge_b));
            }
        }
    }
    output
}

fn is_left_of(a: (f64, f64), b: (f64, f64), p: (f64, f64)) -> bool {
    (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0) >= 0.0
}

fn line_intersection(p1: (f64, f64), p2: (f64, f64), p3: (f64, f64), p4: (f64, f64)) -> (f64, f64) {
    let (x1, y1) = p1;
    let (x2, y2) = p2;
    let (x3, y3) = p3;
    let (x4, y4) = p4;
    let denom = (x1 - x2) * (y3 - y4) - (y1 - y2) * (x3 - x4);
    if denom.abs() < 1e-12 {
        return p2;
    }
    let t = ((x1 - x3) * (y3 - y4) - (y1 - y3) * (x3 - x4)) / denom;
    (x1 + t * (x2 - x1), y1 + t * (y2 - y1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn photo_at(lat: f64, lon: f64) -> PhotoMeta {
        PhotoMeta {
            file_name: String::new(),
            path: String::new(),
            lat,
            lon,
            relative_altitude: Some(80.0),
            yaw_deg: Some(0.0),
            focal_mm: Some(8.8),
            sensor_width_mm: Some(13.2),
            sensor_height_mm: Some(8.8),
            image_width_px: Some(5472),
            image_height_px: Some(3648),
            footprint: None,
            capture_time: None,
            is_blurry: false,
            blur_score: None,
            warnings: Vec::new(),
        }
    }

    fn photo_with_computed_footprint(lat: f64, lon: f64, yaw_deg: f64) -> PhotoMeta {
        let mut p = photo_at(lat, lon);
        p.yaw_deg = Some(yaw_deg);
        p.footprint = geometry::compute_footprint(geometry::FootprintInput {
            lat,
            lon,
            relative_altitude_m: p.relative_altitude.unwrap(),
            focal_mm: p.focal_mm.unwrap(),
            sensor_width_mm: p.sensor_width_mm.unwrap(),
            sensor_height_mm: p.sensor_height_mm.unwrap(),
            yaw_deg,
        });
        p
    }

    #[test]
    fn finds_close_pairs_but_not_far_ones() {
        // ~15.5m grid step (matches pipeline_test.rs's fixture spacing) plus one
        // shot ~700m away. No footprints set, so this only exercises the
        // radius stage (the overlap check gracefully no-ops without one).
        let photos = vec![
            photo_at(10.000_000, 106.000_000),
            photo_at(10.000_000, 106.000_140), // ~15.5m east
            photo_at(10.006_000, 106.000_000), // ~667m north
        ];

        let radius = typical_radius_m(&photos);
        assert!(radius > 0.0, "photos have full metadata, should get a nonzero radius");

        let pairs = find_neighbor_pairs(&photos, radius.max(20.0));
        assert!(pairs.contains(&(0, 1)), "adjacent grid shots should be neighbors");
        assert!(!pairs.contains(&(0, 2)), "the far-away shot should not be a neighbor");
        assert!(!pairs.contains(&(1, 2)));
    }

    #[test]
    fn fewer_than_two_photos_has_no_pairs() {
        assert!(find_neighbor_pairs(&[], 100.0).is_empty());
        assert!(find_neighbor_pairs(&[photo_at(10.0, 106.0)], 100.0).is_empty());
    }

    #[test]
    fn zero_radius_has_no_pairs() {
        let photos = vec![photo_at(10.0, 106.0), photo_at(10.0, 106.0001)];
        assert!(find_neighbor_pairs(&photos, 0.0).is_empty());
    }

    #[test]
    fn identical_position_footprints_fully_overlap() {
        let a = geometry::compute_footprint(geometry::FootprintInput {
            lat: 10.0,
            lon: 106.0,
            relative_altitude_m: 80.0,
            focal_mm: 8.8,
            sensor_width_mm: 13.2,
            sensor_height_mm: 8.8,
            yaw_deg: 0.0,
        })
        .unwrap();
        assert!((overlap_fraction(&a, &a, 10.0, 106.0) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn overlap_area_check_rejects_within_radius_but_barely_touching_photos() {
        // 80m altitude / 8.8mm focal / 13.2x8.8mm sensor -> 120m x 80m footprint
        // (see downsample/pose_graph tests for the same numbers). Shift one photo
        // 110m east: footprints still within the generous diagonal radius, but
        // barely overlap (<30% of either area).
        let a = photo_with_computed_footprint(10.0, 106.0, 0.0);
        let east_shift_deg = 110.0 / (EARTH_RADIUS_M.to_radians() * 10f64.to_radians().cos());
        let b = photo_with_computed_footprint(10.0, 106.0 + east_shift_deg, 0.0);
        let photos = vec![a, b];

        let radius = typical_radius_m(&photos);
        let pairs = find_neighbor_pairs(&photos, radius);
        assert!(pairs.is_empty(), "barely-touching footprints should fail the >30% overlap check");
    }

    #[test]
    fn overlap_area_check_accepts_well_overlapping_photos() {
        let a = photo_with_computed_footprint(10.0, 106.0, 0.0);
        let east_shift_deg = 40.0 / (EARTH_RADIUS_M.to_radians() * 10f64.to_radians().cos());
        let b = photo_with_computed_footprint(10.0, 106.0 + east_shift_deg, 0.0);
        let photos = vec![a, b];

        let radius = typical_radius_m(&photos);
        let pairs = find_neighbor_pairs(&photos, radius);
        assert_eq!(pairs, vec![(0, 1)]);
    }
}
