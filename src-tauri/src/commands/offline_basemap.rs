//! Downloads a regional offline vector basemap (Protomaps/OpenStreetMap-
//! derived) for a user-chosen bounding box, natively in Rust - no bundled
//! `go-pmtiles` sidecar, no CLI dependency on the user's machine (see
//! `public/offline_tiles/README.md`'s history for the manual, dev-only
//! predecessor to this). Reimplements the same job `go-pmtiles extract` does
//! (fetch tiles inside a bbox+zoom from Protomaps' public daily planet build
//! via HTTP range requests, write a new local `.pmtiles` archive) using the
//! `pmtiles` crate's `AsyncPmTilesReader` (remote reads) and
//! `PmTilesWriter`/`PmTilesStreamWriter` (local archive creation).
//!
//! REPLACE semantics: exactly one basemap is active at a time, at a fixed
//! filename under the app-data dir - a new download overwrites it.

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use pmtiles::{AsyncPmTilesReader, PmTilesWriter, TileCoord, TileType};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{mpsc, Semaphore};

use crate::core::tile_math::{self, TileXY};
use crate::models::offline_basemap::{BasemapBbox, OfflineBasemapInfo, OfflineBasemapProgress, OfflineBasemapStage};

const FETCH_CONCURRENCY: usize = 12;
const FETCH_RETRIES: u32 = 3;
/// Pre-flight guard against an accidental huge-area/high-zoom request eating
/// tens of minutes before the user learns their bbox+zoom was too ambitious.
const MAX_TILES: usize = 200_000;

const BASEMAP_DIR: &str = "offline_basemap";
const BASEMAP_FILENAME: &str = "basemap.pmtiles";
const BASEMAP_TMP_FILENAME: &str = "basemap.pmtiles.tmp";
const META_FILENAME: &str = "basemap.meta.json";

fn basemap_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?.join(BASEMAP_DIR);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

async fn find_latest_build(client: &reqwest::Client) -> Result<(String, String), String> {
    let today = chrono::Utc::now().date_naive();
    for days_ago in 0..10 {
        let date = today - chrono::Duration::days(days_ago);
        let date_str = date.format("%Y%m%d").to_string();
        let url = format!("https://build.protomaps.com/{date_str}.pmtiles");
        if let Ok(resp) = client.head(&url).timeout(Duration::from_secs(15)).send().await {
            if resp.status().is_success() {
                return Ok((url, date_str));
            }
        }
    }
    Err("Could not find a recent Protomaps daily build in the last 10 days. Check your internet connection.".to_string())
}

/// Downloads every tile covering `bbox` up to `max_zoom` into a fresh local
/// `.pmtiles` archive, replacing whatever basemap was previously downloaded.
#[tauri::command]
pub async fn download_offline_basemap(app: AppHandle, bbox: BasemapBbox, max_zoom: u8) -> Result<OfflineBasemapInfo, String> {
    let tiles = tile_math::tiles_for_bbox(&bbox, max_zoom)?;
    if tiles.len() > MAX_TILES {
        return Err(format!(
            "Requested area/zoom would need ~{} tiles, over the {MAX_TILES} limit - shrink the bounding box or lower max zoom.",
            tiles.len()
        ));
    }
    let tiles_total = tiles.len() as u32;

    let emit_progress = |stage: OfflineBasemapStage, tiles_done: u32, percent: u8| {
        let _ = app.emit(
            "offline_basemap_progress",
            OfflineBasemapProgress { stage, tiles_done, tiles_total, percent },
        );
    };
    emit_progress(OfflineBasemapStage::LocatingBuild, 0, 0);

    // Per-request timeout matters here specifically: with no timeout, a
    // single slow/hung request blocks indefinitely instead of erroring into
    // a retry loop - confirmed empirically (a 4-tile test fetch took ~8
    // minutes with one stalled request before this was added). 60s (not a
    // tighter value) because the *reader open* itself - a range read against
    // a 100+GB remote object - was observed timing out at 20s on a normal,
    // eventually-successful connection; this is a genuinely slow remote
    // source, not a hang, so the bound needs to be generous.
    let client = reqwest::Client::builder().timeout(Duration::from_secs(60)).build().map_err(|e| e.to_string())?;
    let (build_url, source_build) = find_latest_build(&client).await?;

    // Opening the reader means range-reading a header/directory out of a
    // 100+GB remote object - confirmed empirically to occasionally exceed
    // even a 20s per-request timeout on a perfectly normal connection (not a
    // hang), so this gets the same retry treatment as an individual tile
    // fetch below, rather than failing the whole download on one slow request.
    let mut reader_result = AsyncPmTilesReader::new_with_url(client.clone(), &build_url).await;
    for attempt in 0..FETCH_RETRIES {
        if reader_result.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500 * (attempt as u64 + 1))).await;
        reader_result = AsyncPmTilesReader::new_with_url(client.clone(), &build_url).await;
    }
    let reader = Arc::new(reader_result.map_err(|e| e.to_string())?);

    let dir = basemap_dir(&app)?;
    let tmp_path = dir.join(BASEMAP_TMP_FILENAME);
    let final_path = dir.join(BASEMAP_FILENAME);
    let meta_path = dir.join(META_FILENAME);

    let (tx, mut rx) = mpsc::channel::<(TileXY, Vec<u8>)>(64);
    let failed = Arc::new(AtomicU32::new(0));

    // Writer runs on its own blocking thread and owns the archive the whole
    // time - PmTilesStreamWriter::add_tile takes &mut self, so tile *writes*
    // must be strictly sequential even though tile *fetches* below are
    // parallel; this task is the single consumer that serializes them.
    let write_task = {
        let tmp_path = tmp_path.clone();
        let app = app.clone();
        tauri::async_runtime::spawn_blocking(move || -> Result<u32, String> {
            let file = std::fs::File::create(&tmp_path).map_err(|e| e.to_string())?;
            let mut writer = PmTilesWriter::new(TileType::Mvt)
                .max_zoom(max_zoom)
                .bounds(bbox.min_lon, bbox.min_lat, bbox.max_lon, bbox.max_lat)
                .create(file)
                .map_err(|e| e.to_string())?;

            let mut tiles_done: u32 = 0;
            let mut last_percent: u8 = 0;
            while let Some((tile, data)) = rx.blocking_recv() {
                writer
                    .add_tile(TileCoord::new(tile.z, tile.x, tile.y).map_err(|e| e.to_string())?, &data)
                    .map_err(|e| e.to_string())?;
                tiles_done += 1;
                let percent = ((tiles_done as u64 * 100) / tiles_total.max(1) as u64) as u8;
                if percent > last_percent || tiles_done == tiles_total {
                    last_percent = percent;
                    let _ = app.emit(
                        "offline_basemap_progress",
                        OfflineBasemapProgress { stage: OfflineBasemapStage::DownloadingTiles, tiles_done, tiles_total, percent },
                    );
                }
            }
            writer.finalize().map_err(|e| e.to_string())?;
            Ok(tiles_done)
        })
    };

    // Fetches run in parallel (bounded), each sending its tile bytes to the
    // writer above through the channel; a tile that keeps failing after
    // retries is counted and skipped rather than aborting the whole job.
    let semaphore = Arc::new(Semaphore::new(FETCH_CONCURRENCY));
    let mut fetch_set = tokio::task::JoinSet::new();
    for tile in tiles {
        let reader = reader.clone();
        let semaphore = semaphore.clone();
        let tx = tx.clone();
        let failed = failed.clone();
        fetch_set.spawn(async move {
            let _permit = semaphore.acquire_owned().await.expect("semaphore not closed");
            let coord = match TileCoord::new(tile.z, tile.x, tile.y) {
                Ok(c) => c,
                Err(_) => {
                    failed.fetch_add(1, Ordering::Relaxed);
                    return;
                }
            };
            for attempt in 0..=FETCH_RETRIES {
                match reader.get_tile(coord).await {
                    Ok(Some(bytes)) => {
                        let _ = tx.send((tile, bytes.to_vec())).await;
                        return;
                    }
                    Ok(None) => return, // tile legitimately absent from the source archive
                    Err(_) if attempt < FETCH_RETRIES => {
                        tokio::time::sleep(Duration::from_millis(200 * (attempt as u64 + 1))).await;
                    }
                    Err(_) => {
                        failed.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        });
    }
    drop(tx); // only the per-task clones above keep the channel open now

    while fetch_set.join_next().await.is_some() {}

    let tiles_written = write_task.await.map_err(|e| e.to_string())??;

    std::fs::rename(&tmp_path, &final_path).map_err(|e| e.to_string())?;
    let size_bytes = std::fs::metadata(&final_path).map_err(|e| e.to_string())?.len();

    let info = OfflineBasemapInfo {
        path: final_path.to_string_lossy().to_string(),
        bbox,
        max_zoom,
        tile_count: tiles_written,
        tiles_failed: failed.load(Ordering::Relaxed),
        size_bytes,
        source_build,
        downloaded_at: chrono::Utc::now().to_rfc3339(),
    };
    write_meta(&meta_path, &info)?;
    app.asset_protocol_scope().allow_file(&final_path).map_err(|e| e.to_string())?;
    emit_progress(OfflineBasemapStage::Finalizing, tiles_total, 100);

    Ok(info)
}

fn write_meta(path: &std::path::Path, info: &OfflineBasemapInfo) -> Result<(), String> {
    let json = serde_json::to_string_pretty(info).map_err(|e| e.to_string())?;
    let mut file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    file.write_all(json.as_bytes()).map_err(|e| e.to_string())
}

/// Reads whatever basemap is currently on disk (if any) and re-grants the
/// webview's asset-protocol scope access to it - that scope is in-memory and
/// resets on every app launch, so this must run on frontend mount, not just
/// right after a fresh download.
#[tauri::command]
pub fn get_offline_basemap_info(app: AppHandle) -> Result<Option<OfflineBasemapInfo>, String> {
    let meta_path = basemap_dir(&app)?.join(META_FILENAME);
    if !meta_path.exists() {
        return Ok(None);
    }
    let contents = std::fs::read_to_string(&meta_path).map_err(|e| e.to_string())?;
    let info: OfflineBasemapInfo = serde_json::from_str(&contents).map_err(|e| e.to_string())?;
    if !std::path::Path::new(&info.path).exists() {
        return Ok(None);
    }
    app.asset_protocol_scope().allow_file(&info.path).map_err(|e| e.to_string())?;
    Ok(Some(info))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Not run by default (needs internet + several seconds). Run with:
    ///   cargo test --release offline_basemap_real_fetch -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn offline_basemap_real_fetch() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let bbox = BasemapBbox { min_lon: 18.74, min_lat: 49.55, max_lon: 18.76, max_lat: 49.57 };
        let tiles = tile_math::tiles_for_bbox(&bbox, 3).expect("valid bbox");
        assert!(!tiles.is_empty());

        let client = reqwest::Client::builder().timeout(Duration::from_secs(60)).build().expect("client");
        let (build_url, source_build) = find_latest_build(&client).await.expect("a recent build should exist");
        println!("using build {source_build} at {build_url}");
        let reader = AsyncPmTilesReader::new_with_url(client, &build_url).await.expect("reader opens");

        let out_path = tmp.path().join("test.pmtiles");
        let file = std::fs::File::create(&out_path).expect("create tmp file");
        let mut writer = PmTilesWriter::new(TileType::Mvt)
            .max_zoom(3)
            .bounds(bbox.min_lon, bbox.min_lat, bbox.max_lon, bbox.max_lat)
            .create(file)
            .expect("writer creates");

        let mut written = 0;
        let mut failed = 0;
        for tile in &tiles {
            let coord = TileCoord::new(tile.z, tile.x, tile.y).expect("valid coord");
            match reader.get_tile(coord).await {
                Ok(Some(bytes)) => {
                    writer.add_tile(coord, &bytes).expect("add_tile");
                    written += 1;
                }
                Ok(None) => {}
                Err(_) => failed += 1,
            }
        }
        writer.finalize().expect("finalize");

        println!("tiles requested={}, written={written}, failed={failed}", tiles.len());
        assert!(written > 0, "expected at least one real tile to come back");
        let size = std::fs::metadata(&out_path).unwrap().len();
        assert!(size > 0);
    }
}
