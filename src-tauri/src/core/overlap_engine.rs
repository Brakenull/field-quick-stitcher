//! Overlap & Gap Analysis Engine: rasterizes all photo footprints onto a spatial
//! grid, counts how many footprints cover each cell, and clusters low-coverage
//! cells into "gap" alerts with a GPS centroid.

use rstar::{RTree, RTreeObject, AABB};
use serde_json::json;

use crate::models::inspection_result::{GapAlert, GeoFeature, GeoGeometry};
use crate::models::photo_meta::LonLat;

const EARTH_RADIUS_M: f64 = 6_378_137.0;
/// Hard cap on grid cells so a bad GPS outlier can't blow up memory/CPU.
const MAX_GRID_CELLS: usize = 400_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CoverageLevel {
    Red,
    Yellow,
    Green,
}

impl CoverageLevel {
    fn from_count(count: usize) -> Self {
        if count < 3 {
            CoverageLevel::Red
        } else if count < 5 {
            CoverageLevel::Yellow
        } else {
            CoverageLevel::Green
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            CoverageLevel::Red => "red",
            CoverageLevel::Yellow => "yellow",
            CoverageLevel::Green => "green",
        }
    }
}

struct IndexedFootprint {
    idx: usize,
    envelope: AABB<[f64; 2]>,
}

impl RTreeObject for IndexedFootprint {
    type Envelope = AABB<[f64; 2]>;
    fn envelope(&self) -> Self::Envelope {
        self.envelope
    }
}

fn ring_bbox(ring: &[LonLat]) -> AABB<[f64; 2]> {
    let (mut min_lon, mut min_lat) = (f64::INFINITY, f64::INFINITY);
    let (mut max_lon, mut max_lat) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    for p in ring {
        min_lon = min_lon.min(p[0]);
        max_lon = max_lon.max(p[0]);
        min_lat = min_lat.min(p[1]);
        max_lat = max_lat.max(p[1]);
    }
    AABB::from_corners([min_lon, min_lat], [max_lon, max_lat])
}

/// Ray-casting point-in-polygon test. `ring` may or may not be explicitly closed.
fn point_in_ring(pt: (f64, f64), ring: &[LonLat]) -> bool {
    let n = ring.len();
    if n < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = (ring[i][0], ring[i][1]);
        let (xj, yj) = (ring[j][0], ring[j][1]);
        if ((yi > pt.1) != (yj > pt.1)) && (pt.0 < (xj - xi) * (pt.1 - yi) / (yj - yi) + xi) {
            inside = !inside;
        }
        j = i;
    }
    inside
}

pub struct OverlapAnalysis {
    pub heatmap: Vec<GeoFeature>,
    pub gaps: Vec<GapAlert>,
    pub coverage_area_m2: f64,
}

/// Builds the overlap heatmap grid and clusters low-coverage cells into gap alerts.
///
/// `cell_size_m` should be ~2-5m per the spec; it's auto-coarsened if the resulting
/// grid would exceed [`MAX_GRID_CELLS`].
pub fn analyze(footprints: &[Vec<LonLat>], cell_size_m: f64) -> OverlapAnalysis {
    if footprints.is_empty() {
        return OverlapAnalysis {
            heatmap: Vec::new(),
            gaps: Vec::new(),
            coverage_area_m2: 0.0,
        };
    }

    let overall_bbox = {
        let mut acc = ring_bbox(&footprints[0]);
        for f in &footprints[1..] {
            acc = AABB::from_corners(
                [
                    acc.lower()[0].min(ring_bbox(f).lower()[0]),
                    acc.lower()[1].min(ring_bbox(f).lower()[1]),
                ],
                [
                    acc.upper()[0].max(ring_bbox(f).upper()[0]),
                    acc.upper()[1].max(ring_bbox(f).upper()[1]),
                ],
            );
        }
        acc
    };

    let mid_lat = (overall_bbox.lower()[1] + overall_bbox.upper()[1]) / 2.0;
    let mid_lat_rad = mid_lat.to_radians();
    let m_per_deg_lat = EARTH_RADIUS_M.to_radians();
    let m_per_deg_lon = EARTH_RADIUS_M.to_radians() * mid_lat_rad.cos();

    let width_m = (overall_bbox.upper()[0] - overall_bbox.lower()[0]) * m_per_deg_lon;
    let height_m = (overall_bbox.upper()[1] - overall_bbox.lower()[1]) * m_per_deg_lat;

    let mut effective_cell_m = cell_size_m.max(0.5);
    let mut cols = ((width_m / effective_cell_m).ceil() as usize).max(1);
    let mut rows = ((height_m / effective_cell_m).ceil() as usize).max(1);
    while cols.saturating_mul(rows) > MAX_GRID_CELLS {
        effective_cell_m *= 1.5;
        cols = ((width_m / effective_cell_m).ceil() as usize).max(1);
        rows = ((height_m / effective_cell_m).ceil() as usize).max(1);
    }

    let cell_deg_lon = effective_cell_m / m_per_deg_lon;
    let cell_deg_lat = effective_cell_m / m_per_deg_lat;

    let rtree = RTree::bulk_load(
        footprints
            .iter()
            .enumerate()
            .map(|(idx, ring)| IndexedFootprint {
                idx,
                envelope: ring_bbox(ring),
            })
            .collect(),
    );

    let mut counts = vec![0usize; cols * rows];
    for row in 0..rows {
        let cell_lat_min = overall_bbox.lower()[1] + row as f64 * cell_deg_lat;
        let center_lat = cell_lat_min + cell_deg_lat / 2.0;
        for col in 0..cols {
            let cell_lon_min = overall_bbox.lower()[0] + col as f64 * cell_deg_lon;
            let center_lon = cell_lon_min + cell_deg_lon / 2.0;

            let query = AABB::from_point([center_lon, center_lat]);
            let mut count = 0usize;
            for candidate in rtree.locate_in_envelope_intersecting(&query) {
                if point_in_ring((center_lon, center_lat), &footprints[candidate.idx]) {
                    count += 1;
                }
            }
            counts[row * cols + col] = count;
        }
    }

    let mut heatmap = Vec::with_capacity(cols * rows);
    let mut covered_cells = 0usize;
    for row in 0..rows {
        let lat0 = overall_bbox.lower()[1] + row as f64 * cell_deg_lat;
        let lat1 = lat0 + cell_deg_lat;
        for col in 0..cols {
            let count = counts[row * cols + col];
            if count == 0 {
                continue;
            }
            covered_cells += 1;
            let lon0 = overall_bbox.lower()[0] + col as f64 * cell_deg_lon;
            let lon1 = lon0 + cell_deg_lon;
            let level = CoverageLevel::from_count(count);
            let ring = vec![
                [lon0, lat0],
                [lon1, lat0],
                [lon1, lat1],
                [lon0, lat1],
                [lon0, lat0],
            ];
            heatmap.push(GeoFeature {
                feature_type: "Feature",
                geometry: GeoGeometry::Polygon(vec![ring]),
                properties: json!({
                    "overlapCount": count,
                    "level": level.as_str(),
                }),
            });
        }
    }

    // Flood-fill connected red cells (only within the covered footprint area, so we
    // don't flag the empty margin outside the flight as one giant "gap").
    let mut visited = vec![false; cols * rows];
    let mut gaps = Vec::new();
    let mut next_id = 0u32;
    for row in 0..rows {
        for col in 0..cols {
            let start = row * cols + col;
            if visited[start] {
                continue;
            }
            let count = counts[start];
            let is_red = count > 0 && count < 3;
            if !is_red {
                visited[start] = true;
                continue;
            }

            let mut stack = vec![(row, col)];
            visited[start] = true;
            let mut cluster_cells = Vec::new();
            while let Some((r, c)) = stack.pop() {
                cluster_cells.push((r, c));
                let neighbors = [
                    (r.wrapping_sub(1), c),
                    (r + 1, c),
                    (r, c.wrapping_sub(1)),
                    (r, c + 1),
                ];
                for (nr, nc) in neighbors {
                    if nr >= rows || nc >= cols {
                        continue;
                    }
                    let nidx = nr * cols + nc;
                    if visited[nidx] {
                        continue;
                    }
                    let ncount = counts[nidx];
                    if ncount > 0 && ncount < 3 {
                        visited[nidx] = true;
                        stack.push((nr, nc));
                    }
                }
            }

            let cell_count = cluster_cells.len();
            let mut sum_lat = 0.0;
            let mut sum_lon = 0.0;
            for &(r, c) in &cluster_cells {
                let lat = overall_bbox.lower()[1] + (r as f64 + 0.5) * cell_deg_lat;
                let lon = overall_bbox.lower()[0] + (c as f64 + 0.5) * cell_deg_lon;
                sum_lat += lat;
                sum_lon += lon;
            }
            let gap = GapAlert {
                id: next_id,
                lat: sum_lat / cell_count as f64,
                lon: sum_lon / cell_count as f64,
                cell_count,
                area_m2: cell_count as f64 * effective_cell_m * effective_cell_m,
            };
            next_id += 1;
            gaps.push(gap);
        }
    }
    gaps.sort_by(|a, b| b.area_m2.partial_cmp(&a.area_m2).unwrap());

    OverlapAnalysis {
        heatmap,
        gaps,
        coverage_area_m2: covered_cells as f64 * effective_cell_m * effective_cell_m,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(lon0: f64, lat0: f64, size_deg: f64) -> Vec<LonLat> {
        vec![
            [lon0, lat0],
            [lon0 + size_deg, lat0],
            [lon0 + size_deg, lat0 + size_deg],
            [lon0, lat0 + size_deg],
            [lon0, lat0],
        ]
    }

    #[test]
    fn point_in_ring_basic() {
        let ring = square(0.0, 0.0, 1.0);
        assert!(point_in_ring((0.5, 0.5), &ring));
        assert!(!point_in_ring((1.5, 0.5), &ring));
    }

    #[test]
    fn overlapping_squares_produce_higher_counts_in_the_middle() {
        // Five overlapping ~50m squares stepped east, so the center column has full
        // overlap (5) while the far edges only get partial coverage.
        let step = 0.0002; // ~22m at the equator
        let size = 0.0009; // ~100m
        let footprints: Vec<_> = (0..5).map(|i| square(i as f64 * step, 0.0, size)).collect();

        let result = analyze(&footprints, 5.0);
        assert!(!result.heatmap.is_empty());

        let max_count = result
            .heatmap
            .iter()
            .filter_map(|f| f.properties.get("overlapCount").and_then(|v| v.as_u64()))
            .max()
            .unwrap();
        assert_eq!(max_count, 5);
    }

    #[test]
    fn sparse_single_footprint_is_all_red_and_reported_as_gap() {
        let footprints = vec![square(0.0, 0.0, 0.001)];
        let result = analyze(&footprints, 5.0);
        assert!(!result.gaps.is_empty());
        assert_eq!(result.gaps[0].cell_count, {
            result
                .heatmap
                .iter()
                .filter(|f| f.properties.get("level").and_then(|v| v.as_str()) == Some("red"))
                .count()
        });
    }

    #[test]
    fn empty_input_produces_empty_output() {
        let result = analyze(&[], 5.0);
        assert!(result.heatmap.is_empty());
        assert!(result.gaps.is_empty());
        assert_eq!(result.coverage_area_m2, 0.0);
    }
}
