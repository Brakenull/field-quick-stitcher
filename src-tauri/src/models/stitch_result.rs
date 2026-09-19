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

/// Whether the MatchingPairs stage caps each photo's candidate-neighbor
/// count (`core::stitching::neighbor_index::MAX_NEIGHBORS_PER_PHOTO`) or
/// matches every pair above the overlap threshold - see that module's doc
/// comment for the mechanics and `pipeline::run`'s doc comment for the
/// trade-off this exposes. `Capped` bounds MatchingPairs runtime to roughly
/// photo-count-only, independent of how much the survey's flight lines
/// overlap themselves, at the cost of dropping some pose-graph edge
/// redundancy; `Uncapped` matches every pair the overlap threshold allows,
/// for a user who wants maximum pose-graph robustness and is willing to
/// trade time for it (confirmed 5-8x more pairs, hours instead of tens of
/// minutes, on real dense surveys). `Capped` is the default.
#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NeighborCap {
    Capped,
    Uncapped,
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
    pub neighbor_cap: NeighborCap,
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
