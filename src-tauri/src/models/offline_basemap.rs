use serde::{Deserialize, Serialize};

/// A geographic area the user wants an offline basemap for, in plain lon/lat
/// degrees (not a tile-space bbox - `core::tile_math` converts).
#[derive(Deserialize, Serialize, Clone, Copy, Debug)]
#[serde(rename_all = "camelCase")]
pub struct BasemapBbox {
    pub min_lon: f64,
    pub min_lat: f64,
    pub max_lon: f64,
    pub max_lat: f64,
}

/// One step of `download_offline_basemap`, in the order it actually runs -
/// mirrors `StitchStage`'s reasoning: the UI needs to show which phase is
/// running (locating a build can itself take a few seconds of HEAD probing)
/// rather than a bar that sits at 0% while nothing visibly happens.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum OfflineBasemapStage {
    LocatingBuild,
    DownloadingTiles,
    Finalizing,
}

#[derive(Serialize, Clone, Copy, Debug)]
#[serde(rename_all = "camelCase")]
pub struct OfflineBasemapProgress {
    pub stage: OfflineBasemapStage,
    pub tiles_done: u32,
    pub tiles_total: u32,
    pub percent: u8,
}

/// Both the `download_offline_basemap`/`get_offline_basemap_info` IPC return
/// type AND the shape round-tripped to/from the on-disk `basemap.meta.json`
/// sidecar next to `basemap.pmtiles` - the archive itself doesn't carry the
/// requested bbox/source-build/download-time back out conveniently, so those
/// live in this small sidecar instead.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct OfflineBasemapInfo {
    pub path: String,
    pub bbox: BasemapBbox,
    pub max_zoom: u8,
    pub tile_count: u32,
    pub tiles_failed: u32,
    pub size_bytes: u64,
    /// The Protomaps daily build date (`YYYYMMDD`) the tiles were pulled from.
    pub source_build: String,
    /// RFC3339 timestamp of when the download finished.
    pub downloaded_at: String,
}
