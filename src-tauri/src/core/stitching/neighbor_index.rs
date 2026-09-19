//! GPS-guided neighbor lookup: restricts feature matching to photo pairs whose
//! ground footprints are close enough to plausibly overlap, instead of the
//! O(N^2) all-pairs comparison a naive stitcher would do. Three-stage filter:
//! a cheap R-tree distance radius first (spec section 2b: `R ≈ W_ground`,
//! keeps this sub-O(N^2)), an exact footprint-polygon overlap-area check
//! (section 6.1.b: >30% overlap) on just the surviving candidates, then a
//! per-photo top-K cap (see [`MAX_NEIGHBORS_PER_PHOTO`]) not in the spec but
//! needed in practice - a real dense-grid ("cross-hatch") survey with high
//! front+side overlap pushed a photo's neighbor count past 80 (172 photos,
//! 7,210 pairs, median 85/photo), and at ~1.8-2s/pair serialized through one
//! CPU matcher session (`onnx_matcher.rs`), that's ~3.7 hours in the
//! MatchingPairs stage alone for what pose-graph alignment only needed a
//! handful of strong edges per photo to solve just as well.

use std::collections::HashSet;

use rstar::{RTree, RTreeObject, AABB};

use crate::core::geometry;
use crate::models::photo_meta::{LonLat, PhotoMeta};

const EARTH_RADIUS_M: f64 = 6_378_137.0;
/// Minimum footprint-overlap fraction (of the smaller photo's area) to treat
/// two photos as neighbors worth matching, per spec section 6.1.b.
const MIN_OVERLAP_FRACTION: f64 = 0.3;

/// Max neighbors kept per photo, ranked by overlap fraction, after the
/// >30% threshold above - see the module doc comment for why this exists.
/// Pose-graph alignment (`pose_graph.rs`) needs enough edges for
/// connectivity and a bit of redundancy, not an edge for literally every
/// pair above the overlap threshold; 10 comfortably covers a normal
/// single-pass strip survey's handful of along-track + cross-track
/// neighbors while still bounding a dense-grid survey's matching cost.
const MAX_NEIGHBORS_PER_PHOTO: usize = 10;

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

    // Every photo reaching this function already has a computed footprint
    // (the pipeline only calls it on that subset - see `pipeline.rs`), which
    // itself requires lat/lon to have been `Some` when the footprint was
    // built - so unwrapping here is safe, not a guess.
    let mid_lat = photos.iter().map(|p| p.lat.expect("footprint-bearing photo has GPS")).sum::<f64>() / photos.len() as f64;
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
            .map(|(idx, p)| IndexedCenter {
                idx,
                lon: p.lon.expect("footprint-bearing photo has GPS"),
                lat: p.lat.expect("footprint-bearing photo has GPS"),
            })
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

    let scored: Vec<(usize, usize, f64)> = pairs
        .into_iter()
        .filter_map(|(i, j)| {
            let score = overlap_score(&photos[i], &photos[j]);
            (score > MIN_OVERLAP_FRACTION).then_some((i, j, score))
        })
        .collect();

    let mut result: Vec<_> = cap_neighbors_per_photo(&scored, photos.len(), MAX_NEIGHBORS_PER_PHOTO).into_iter().collect();
    result.sort_unstable();
    result
}

/// The overlap-fraction ranking score for a pair: the real footprint overlap
/// fraction when both photos have one, or `f64::INFINITY` when either
/// doesn't - so a pair this can't actually measure always passes the
/// threshold and always wins its spot in the top-K cap below, gracefully
/// degrading to radius-only exactly like the check this replaced.
fn overlap_score(a: &PhotoMeta, b: &PhotoMeta) -> f64 {
    match (&a.footprint, &b.footprint) {
        // A footprint can't exist without lat/lon (see `commands::inspect::parse_one`).
        (Some(fa), Some(fb)) => overlap_fraction(fa, fb, a.lat.expect("footprint implies GPS"), a.lon.expect("footprint implies GPS")),
        _ => f64::INFINITY,
    }
}

/// Keeps a pair only if it's among either endpoint's `max_neighbors`
/// strongest-scoring overlaps - a union, not an intersection, so a photo's
/// own best neighbor is never dropped just because that neighbor has enough
/// *other* strong overlaps to not reciprocate. Bounds each photo's own
/// candidate list to `max_neighbors`, but a given photo can still end up
/// with more than that many kept pairs if other photos rank it highly - the
/// goal is capping the worst-case blowup (see module doc comment), not
/// enforcing an exact per-photo edge count.
fn cap_neighbors_per_photo(scored: &[(usize, usize, f64)], photo_count: usize, max_neighbors: usize) -> HashSet<(usize, usize)> {
    let mut per_photo: Vec<Vec<(f64, usize)>> = vec![Vec::new(); photo_count];
    for &(i, j, score) in scored {
        per_photo[i].push((score, j));
        per_photo[j].push((score, i));
    }

    let mut kept = HashSet::new();
    for (idx, neighbors) in per_photo.iter_mut().enumerate() {
        neighbors.sort_unstable_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        for &(_, other) in neighbors.iter().take(max_neighbors) {
            kept.insert(if idx < other { (idx, other) } else { (other, idx) });
        }
    }
    kept
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
            lat: Some(lat),
            lon: Some(lon),
            relative_altitude: Some(80.0),
            absolute_altitude: None,
            yaw_deg: Some(0.0),
            focal_mm: Some(8.8),
            sensor_width_mm: Some(13.2),
            sensor_height_mm: Some(8.8),
            image_width_px: Some(5472),
            image_height_px: Some(3648),
            footprint: None,
            capture_time: None,
            sort_key: crate::models::photo_meta::CaptureOrderKey::Unresolved,
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

    #[test]
    fn cap_neighbors_per_photo_keeps_a_pair_either_endpoint_ranks_highly() {
        // Photo 0's own top-2 are 1 and 2 (0.9, 0.8), ranking 3 (0.7) out -
        // but photo 3's own top-2 are 0 (0.7) and 2 (0.95), so it reciprocates.
        let scored = vec![(0, 1, 0.9), (0, 2, 0.8), (0, 3, 0.7), (2, 3, 0.95)];
        let kept = cap_neighbors_per_photo(&scored, 4, 2);
        assert!(kept.contains(&(0, 1)));
        assert!(kept.contains(&(0, 2)));
        assert!(kept.contains(&(0, 3)), "photo 3 should reciprocate even though photo 0 ranked it 3rd");
        assert!(kept.contains(&(2, 3)));
    }

    #[test]
    fn cap_neighbors_per_photo_drops_a_pair_neither_endpoint_ranks_highly() {
        // Photo 0 has 3 partners stronger than 4; photo 4 has 2 partners
        // stronger than 0. With max_neighbors=2, neither side keeps (0, 4).
        let scored = vec![(0, 1, 0.9), (0, 2, 0.85), (0, 3, 0.8), (0, 4, 0.3), (4, 5, 0.95), (4, 6, 0.9)];
        let kept = cap_neighbors_per_photo(&scored, 7, 2);
        assert!(!kept.contains(&(0, 4)));
        assert!(kept.contains(&(0, 1)));
        assert!(kept.contains(&(4, 5)));
    }

    #[test]
    fn find_neighbor_pairs_bounds_a_dense_grid_survey() {
        // 5x5 grid, ~20m spacing, every photo overlapping most others within
        // the radius (mirrors the real dense "cross-hatch" survey that
        // motivated the cap: median 85 neighbors/photo, unbounded). Without
        // the cap this produces a near-complete graph; with it, no photo
        // should end up with a lot more than MAX_NEIGHBORS_PER_PHOTO edges.
        let mut photos = Vec::new();
        for row in 0..5 {
            for col in 0..5 {
                let lat = 10.0 + row as f64 * 0.00014; // ~15.5m spacing
                let lon = 106.0 + col as f64 * 0.00014;
                photos.push(photo_with_computed_footprint(lat, lon, 0.0));
            }
        }

        let radius = typical_radius_m(&photos);
        let pairs = find_neighbor_pairs(&photos, radius);

        let mut counts = vec![0usize; photos.len()];
        for &(i, j) in &pairs {
            counts[i] += 1;
            counts[j] += 1;
        }
        let max_count = counts.iter().copied().max().unwrap_or(0);
        assert!(max_count <= MAX_NEIGHBORS_PER_PHOTO * 2, "expected the cap to bound per-photo neighbor count, got {max_count}");
    }
}
