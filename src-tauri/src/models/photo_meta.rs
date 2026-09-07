use serde::Serialize;

/// A single [lon, lat] vertex, GeoJSON order.
pub type LonLat = [f64; 2];

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PhotoMeta {
    pub file_name: String,
    pub path: String,
    pub lat: f64,
    pub lon: f64,
    /// Height above ground (meters), from DJI XMP RelativeAltitude when available.
    pub relative_altitude: Option<f64>,
    /// Camera/gimbal heading in degrees, 0 = North, clockwise.
    pub yaw_deg: Option<f64>,
    pub focal_mm: Option<f64>,
    pub sensor_width_mm: Option<f64>,
    pub sensor_height_mm: Option<f64>,
    pub image_width_px: Option<u32>,
    pub image_height_px: Option<u32>,
    /// Ground footprint polygon (4 corners + closing vertex), [lon, lat] pairs, or None if
    /// altitude/yaw/sensor data was insufficient to compute it.
    pub footprint: Option<Vec<LonLat>>,
    pub capture_time: Option<String>,
    pub is_blurry: bool,
    pub blur_score: Option<f64>,
    /// Non-fatal issues encountered while reading this photo's metadata.
    pub warnings: Vec<String>,
}
