use serde::{Deserialize, Serialize};

use super::photo_meta::LonLat;

/// Which feature-matching backend Quick Stitch should run with (spec section
/// 6): the default ONNX (SuperPoint extractor + LightGlue matcher, DirectML-
/// accelerated) path, or the CPU-only ORB path kept dormant-no-more in
/// `core/stitching/features.rs`/`matching.rs` for machines without a usable
/// GPU or where ONNX Runtime itself fails to load.
#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum StitchBackend {
    Onnx,
    Orb,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct StitchResult {
    /// Path to the written GeoTIFF (COG-ready: DEFLATE-compressed, WGS84).
    pub geotiff_path: String,
    /// Path to a plain PNG rendering of the same mosaic, for quick display in
    /// MapLibre (which can't load a GeoTIFF directly) via an `image` source.
    pub preview_path: String,
    /// The preview image's 4 corners in MapLibre `image` source order:
    /// top-left, top-right, bottom-right, bottom-left.
    pub preview_corners: [LonLat; 4],
    pub backend: StitchBackend,
    pub photos_used: usize,
    pub photos_skipped: usize,
    pub confident_pairs: usize,
    pub duration_ms: u128,
    pub warnings: Vec<String>,
}

/// One step of the Quick Stitch pipeline, in the order it actually runs.
/// Reported over the `stitch_progress` event alongside a 0-100 percent
/// *within that stage*, so the UI can show which phase is running instead of
/// a progress bar that reaches 100% after downsampling and then goes silent
/// for the rest of the run (feature matching, pose alignment, and mosaic
/// compositing - typically the bulk of the wall-clock time).
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum StitchStage {
    Downsampling,
    DetectingFeatures,
    MatchingPairs,
    AligningPoses,
    Compositing,
    Exporting,
}

#[derive(Serialize, Clone, Copy, Debug)]
#[serde(rename_all = "camelCase")]
pub struct StitchProgress {
    pub stage: StitchStage,
    pub percent: u8,
}
