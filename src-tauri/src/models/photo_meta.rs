use serde::Serialize;

/// A single [lon, lat] vertex, GeoJSON order.
pub type LonLat = [f64; 2];

/// Best-effort chronological ordering key for placing a photo into the flight
/// path, in priority order: a parsed capture timestamp (with sub-second
/// precision when available), a numeric sequence extracted from the file
/// name, the file's last-modified time, and finally nothing at all. Variant
/// *declaration* order is the sort priority (derived `Ord` compares the
/// discriminant first) - photos resolved via an earlier tier always sort
/// before every photo that fell back to a later one, and `Unresolved` photos
/// always land at the end, rather than (via raw `Option`/string comparison)
/// jumping to the front or interleaving with genuinely-timestamped photos.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum CaptureOrderKey {
    Timestamp(i64),
    FilenameSequence(u64),
    FileModified(i64),
    Unresolved,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PhotoMeta {
    pub file_name: String,
    pub path: String,
    /// `None` when the photo has no usable GPS EXIF - kept in the list rather
    /// than dropped, so a GPS dropout mid-flight is visible in `photos`
    /// (`Metrics::photos_with_gps` counts the `Some` ones) instead of being
    /// silently filtered out along with everything derived from it.
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    /// Height above ground (meters), from DJI XMP RelativeAltitude when available.
    pub relative_altitude: Option<f64>,
    /// Absolute altitude above the WGS84 ellipsoid/sea level (meters), from
    /// DJI XMP AbsoluteAltitude - captured for a future terrain-correction
    /// pass (comparing this against a ground-elevation DEM to get an
    /// effective above-ground-level altitude over hilly terrain) but **not
    /// currently used anywhere**: `geometry::compute_footprint` still assumes
    /// flat ground at `relative_altitude`. See `1-inspect.md` section 7.
    pub absolute_altitude: Option<f64>,
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
    /// Not sent to the frontend (sorting only) - see `CaptureOrderKey`.
    #[serde(skip)]
    pub sort_key: CaptureOrderKey,
    pub is_blurry: bool,
    pub blur_score: Option<f64>,
    /// Non-fatal issues encountered while reading this photo's metadata.
    pub warnings: Vec<String>,
}
