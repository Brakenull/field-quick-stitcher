use serde::Serialize;

use super::photo_meta::PhotoMeta;

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct GapAlert {
    pub id: u32,
    /// Approximate centroid of the gap cluster.
    pub lat: f64,
    pub lon: f64,
    pub cell_count: usize,
    pub area_m2: f64,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct BlurAlert {
    pub file_name: String,
    pub lat: f64,
    pub lon: f64,
    pub blur_score: f64,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Metrics {
    pub total_photos: usize,
    pub photos_with_gps: usize,
    pub blurry_count: usize,
    pub gap_count: usize,
    pub avg_altitude_m: Option<f64>,
    pub coverage_area_m2: f64,
    pub scan_duration_ms: u128,
}

/// A single GeoJSON Feature, kept generic enough to serialize both the heatmap grid cells and
/// the flight path/photo points without pulling in a full GeoJSON crate.
#[derive(Serialize, Clone, Debug)]
pub struct GeoFeature {
    #[serde(rename = "type")]
    pub feature_type: &'static str,
    pub geometry: GeoGeometry,
    pub properties: serde_json::Value,
}

#[derive(Serialize, Clone, Debug)]
#[serde(tag = "type", content = "coordinates")]
pub enum GeoGeometry {
    Point(super::photo_meta::LonLat),
    LineString(Vec<super::photo_meta::LonLat>),
    Polygon(Vec<Vec<super::photo_meta::LonLat>>),
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct InspectionResult {
    pub photos: Vec<PhotoMeta>,
    /// Ordered by capture time, one point per photo with GPS.
    pub flight_path: Vec<GeoFeature>,
    /// Overlap-count grid cells, colored red/yellow/green by the frontend via overlap_count.
    pub heatmap: Vec<GeoFeature>,
    pub gaps: Vec<GapAlert>,
    pub blur_alerts: Vec<BlurAlert>,
    pub metrics: Metrics,
}
