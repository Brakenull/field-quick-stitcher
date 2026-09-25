//! Downloads a regional offline vector basemap (Protomaps/OpenStreetMap-
//! derived) for a user-chosen bounding box, natively in Rust: fetches every
//! z/x/y tile inside a bbox+zoom from the hosted Protomaps tile API
//! (https://protomaps.com/api) and writes them into a new local `.pmtiles`
//! archive with the `pmtiles` crate's `PmTilesWriter`.
//!
//! The API replaced range-reading Protomaps' public daily planet build
//! (build.protomaps.com) directly: that's one uncacheable ~138GB object on a
//! shared host whose throughput was measured swinging between 4KB/s and
//! 230KB/s, with individual requests stalling for 20s+. The API serves each
//! tile as its own small, CDN-cached response instead, in the same v4
//! "Basemap Layers" schema `@protomaps/basemaps` styles expect.
//!
//! REPLACE semantics: exactly one basemap is active at a time, at a fixed
//! filename under the app-data dir - a new download overwrites it.

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use pmtiles::{PmTilesWriter, TileCoord, TileType};
use reqwest::StatusCode;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{mpsc, Semaphore};

use crate::core::tile_math::{self, TileXY};
use crate::models::offline_basemap::{BasemapBbox, OfflineBasemapInfo, OfflineBasemapProgress, OfflineBasemapStage};
use crate::AppState;

const API_TILE_BASE_URL: &str = "https://api.protomaps.com/tiles/v4";
/// Recorded as `OfflineBasemapInfo::source_build` for API downloads.
const API_SOURCE_NAME: &str = "protomaps-api-v4";

/// Benchmarked on a real survey bbox (`basemap_download_bench`) against the
/// old daily-build source: 12 -> 32 cut the fetch stage ~30%, 64 gained
/// nothing more.
const FETCH_CONCURRENCY: usize = 32;
/// Stall detector: a request that receives *no bytes at all* for this long
/// is dropped and retried. Deliberately an idle timeout, not a tight total
/// one - a short total timeout cancels slow-but-progressing requests, throws
/// their partial bytes away and restarts them, so on a slow connection tiles
/// never finish at all (observed with the old daily-build source).
const READ_IDLE_TIMEOUT: Duration = Duration::from_secs(20);
/// Hard upper bound per request, only as a backstop against a connection
/// that keeps trickling bytes forever - READ_IDLE_TIMEOUT is the real guard.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(180);
const TILE_RETRIES: u32 = 5;
/// Pre-flight guard against an accidental huge-area/high-zoom request eating
/// tens of minutes before the user learns their bbox+zoom was too ambitious.
const MAX_TILES: usize = 200_000;

const BASEMAP_DIR: &str = "offline_basemap";
const BASEMAP_FILENAME: &str = "basemap.pmtiles";
const BASEMAP_TMP_FILENAME: &str = "basemap.pmtiles.tmp";
const META_FILENAME: &str = "basemap.meta.json";

fn basemap_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("failed to resolve app-data directory: {e}"))?
        .join(BASEMAP_DIR);
    std::fs::create_dir_all(&dir).map_err(|e| format!("failed to create {}: {e}", dir.display()))?;
    Ok(dir)
}

/// The Protomaps API key: a runtime `PROTOMAP_KEY` env var wins (handy for
/// swapping keys without a rebuild), otherwise the one `build.rs` embedded
/// from the repo-root `.env` at compile time.
fn api_key() -> Result<String, String> {
    std::env::var("PROTOMAP_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty())
        .or_else(|| option_env!("PROTOMAP_KEY").filter(|k| !k.trim().is_empty()).map(str::to_string))
        .map(|k| k.trim().to_string())
        .ok_or_else(|| "No Protomaps API key configured - set PROTOMAP_KEY in the project's .env and rebuild.".to_string())
}

fn build_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .read_timeout(READ_IDLE_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|e| format!("failed to build HTTP client: {e}"))
}

/// Exponential backoff between tile retries: 250ms, 500ms, 1s, 2s, 4s.
fn retry_backoff(attempt: u32) -> Duration {
    Duration::from_millis(250 << attempt.min(4))
}

enum TileError {
    /// Retrying can't help (bad/revoked key, origin not allowed) - carries a
    /// user-facing message.
    Fatal(String),
    Transient,
}

/// One GET for one tile. `Ok(None)` = the API has no tile there (404/204),
/// which is legitimate (e.g. past its z15 max), not a failure.
///
/// The body is raw, uncompressed MVT - reqwest is built without its `gzip`
/// feature, so it never advertises Accept-Encoding and the API answers with
/// identity encoding. That matters: PmTilesWriter::new(Mvt) gzip-compresses
/// whatever it's given, and handing it already-gzipped bytes double-gzips
/// them into an archive every reader decodes as garbage (hit once before
/// with the daily-build source - it rendered as an empty background).
async fn fetch_tile_once(client: &reqwest::Client, key: &str, tile: TileXY) -> Result<Option<Vec<u8>>, TileError> {
    let mut url = reqwest::Url::parse(&format!("{API_TILE_BASE_URL}/{}/{}/{}.mvt", tile.z, tile.x, tile.y))
        .map_err(|e| TileError::Fatal(format!("invalid tile URL: {e}")))?;
    url.query_pairs_mut().append_pair("key", key);
    let resp = client.get(url).send().await.map_err(|_| TileError::Transient)?;
    match resp.status() {
        StatusCode::OK => resp.bytes().await.map(|b| Some(b.to_vec())).map_err(|_| TileError::Transient),
        StatusCode::NO_CONTENT | StatusCode::NOT_FOUND => Ok(None),
        status @ (StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) => {
            let body = resp.text().await.unwrap_or_default();
            Err(TileError::Fatal(format!(
                "Protomaps API rejected the request (HTTP {}: {}) - check PROTOMAP_KEY and the key's allowed origins.",
                status.as_u16(),
                body.trim()
            )))
        }
        // 429 (rate limited), 5xx and anything unexpected: worth another try.
        _ => Err(TileError::Transient),
    }
}

async fn fetch_tile(client: &reqwest::Client, key: &str, tile: TileXY) -> Result<Option<Vec<u8>>, TileError> {
    let mut attempt = 0;
    loop {
        match fetch_tile_once(client, key, tile).await {
            Err(TileError::Transient) if attempt < TILE_RETRIES => {
                tokio::time::sleep(retry_backoff(attempt)).await;
                attempt += 1;
            }
            other => return other,
        }
    }
}

/// Downloads every tile covering `bbox` up to `max_zoom` into a fresh local
/// `.pmtiles` archive, replacing whatever basemap was previously downloaded.
#[tauri::command]
pub async fn download_offline_basemap(
    app: AppHandle,
    state: State<'_, AppState>,
    bbox: BasemapBbox,
    max_zoom: u8,
) -> Result<OfflineBasemapInfo, String> {
    // Guards the fixed tmp/final filenames below - two overlapping downloads
    // would otherwise race on them (one's rename fails because the other
    // already moved the file away). try_lock (not .lock().await): reject a
    // second concurrent call outright rather than silently queuing it, since
    // queuing would mean starting a whole redundant download only to have it
    // immediately overwritten anyway (REPLACE semantics - one basemap total).
    let _download_guard = state
        .offline_basemap_download_lock
        .try_lock()
        .map_err(|_| "An offline map download is already in progress - wait for it to finish.".to_string())?;

    let key = api_key()?;
    let mut tiles = tile_math::tiles_for_bbox(&bbox, max_zoom)?;
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
    emit_progress(OfflineBasemapStage::Connecting, 0, 0);

    let client = build_client()?;

    // Fetch the single z0 world tile on its own first: it doubles as a key/
    // connectivity check, so a bad key fails fast with the API's own message
    // instead of 30-odd parallel requests all failing into a "0 tiles" archive.
    let world_tile = TileXY { z: 0, x: 0, y: 0 };
    tiles.retain(|t| *t != world_tile);
    let world_data = match fetch_tile(&client, &key, world_tile).await {
        Ok(data) => data,
        Err(TileError::Fatal(msg)) => return Err(msg),
        Err(TileError::Transient) => {
            return Err("Couldn't reach the Protomaps tile API - check your internet connection.".to_string())
        }
    };

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
            let file = std::fs::File::create(&tmp_path)
                .map_err(|e| format!("failed to create {}: {e}", tmp_path.display()))?;
            let mut writer = PmTilesWriter::new(TileType::Mvt)
                .max_zoom(max_zoom)
                .bounds(bbox.min_lon, bbox.min_lat, bbox.max_lon, bbox.max_lat)
                .create(file)
                .map_err(|e| format!("failed to initialize pmtiles writer: {e}"))?;

            let mut tiles_done: u32 = 0;
            let mut last_percent: u8 = 0;
            while let Some((tile, data)) = rx.blocking_recv() {
                let coord = TileCoord::new(tile.z, tile.x, tile.y)
                    .map_err(|e| format!("invalid tile coordinate {}/{}/{}: {e}", tile.z, tile.x, tile.y))?;
                writer
                    .add_tile(coord, &data)
                    .map_err(|e| format!("failed to write tile {}/{}/{} to archive: {e}", tile.z, tile.x, tile.y))?;
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
            writer.finalize().map_err(|e| format!("failed to finalize pmtiles archive: {e}"))?;
            Ok(tiles_done)
        })
    };

    if let Some(data) = world_data {
        let _ = tx.send((world_tile, data)).await;
    }

    // Fetches run in parallel (bounded), each sending its tile bytes to the
    // writer above through the channel; a tile that keeps failing after
    // retries is counted and skipped rather than aborting the whole job.
    let semaphore = Arc::new(Semaphore::new(FETCH_CONCURRENCY));
    let client = Arc::new(client);
    let key = Arc::new(key);
    let mut fetch_set = tokio::task::JoinSet::new();
    for tile in tiles {
        let client = client.clone();
        let key = key.clone();
        let semaphore = semaphore.clone();
        let tx = tx.clone();
        let failed = failed.clone();
        fetch_set.spawn(async move {
            let _permit = semaphore.acquire_owned().await.expect("semaphore not closed");
            match fetch_tile(&client, &key, tile).await {
                Ok(Some(data)) => {
                    let _ = tx.send((tile, data)).await;
                }
                Ok(None) => {} // no tile there upstream - legitimate, not a failure
                Err(_) => {
                    failed.fetch_add(1, Ordering::Relaxed);
                }
            }
        });
    }
    drop(tx); // only the per-task clones above keep the channel open now

    while fetch_set.join_next().await.is_some() {}

    let tiles_written = write_task.await.map_err(|e| format!("writer task panicked: {e}"))??;

    std::fs::rename(&tmp_path, &final_path)
        .map_err(|e| format!("failed to move {} to {}: {e}", tmp_path.display(), final_path.display()))?;
    let size_bytes = std::fs::metadata(&final_path)
        .map_err(|e| format!("failed to stat {}: {e}", final_path.display()))?
        .len();

    let info = OfflineBasemapInfo {
        path: final_path.to_string_lossy().to_string(),
        bbox,
        max_zoom,
        tile_count: tiles_written,
        tiles_failed: failed.load(Ordering::Relaxed),
        size_bytes,
        source_build: API_SOURCE_NAME.to_string(),
        downloaded_at: chrono::Utc::now().to_rfc3339(),
    };
    write_meta(&meta_path, &info)?;
    app.asset_protocol_scope()
        .allow_file(&final_path)
        .map_err(|e| format!("failed to grant asset-protocol access to {}: {e}", final_path.display()))?;
    emit_progress(OfflineBasemapStage::Finalizing, tiles_total, 100);

    Ok(info)
}

fn write_meta(path: &std::path::Path, info: &OfflineBasemapInfo) -> Result<(), String> {
    let json = serde_json::to_string_pretty(info).map_err(|e| format!("failed to serialize basemap metadata: {e}"))?;
    let mut file = std::fs::File::create(path).map_err(|e| format!("failed to create {}: {e}", path.display()))?;
    file.write_all(json.as_bytes())
        .map_err(|e| format!("failed to write {}: {e}", path.display()))
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
    let contents = std::fs::read_to_string(&meta_path).map_err(|e| format!("failed to read {}: {e}", meta_path.display()))?;
    let info: OfflineBasemapInfo =
        serde_json::from_str(&contents).map_err(|e| format!("failed to parse {}: {e}", meta_path.display()))?;
    if !std::path::Path::new(&info.path).exists() {
        return Ok(None);
    }
    app.asset_protocol_scope()
        .allow_file(&info.path)
        .map_err(|e| format!("failed to grant asset-protocol access to {}: {e}", info.path))?;
    Ok(Some(info))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mirrors bboxOfPhotos in src/lib/flightBbox.ts.
    fn bbox_of_folder(dir: &str) -> BasemapBbox {
        let (mut min_lon, mut min_lat, mut max_lon, mut max_lat) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for e in std::fs::read_dir(dir).expect("dataset dir") {
            let p = e.unwrap().path();
            let m = crate::core::metadata_parser::parse_photo(&p);
            if let (Some(lat), Some(lon)) = (m.lat, m.lon) {
                min_lon = min_lon.min(lon);
                max_lon = max_lon.max(lon);
                min_lat = min_lat.min(lat);
                max_lat = max_lat.max(lat);
            }
        }
        let lon_pad = ((max_lon - min_lon) * 0.15).max(0.01);
        let lat_pad = ((max_lat - min_lat) * 0.15).max(0.01);
        BasemapBbox { min_lon: min_lon - lon_pad, min_lat: min_lat - lat_pad, max_lon: max_lon + lon_pad, max_lat: max_lat + lat_pad }
    }

    /// Same fetch/write path as `download_offline_basemap` minus the Tauri
    /// app handle: fetches a real area from the API, checks every tile is
    /// plain (not gzipped) MVT, and writes a temp archive. Not run by default
    /// (needs internet + a PROTOMAP_KEY). Run with:
    ///   BASEMAP_BENCH_DIR=<photo folder> cargo test --release basemap_download_bench -- --ignored --nocapture
    /// BASEMAP_BENCH_ZOOM overrides the default max zoom 12; without
    /// BASEMAP_BENCH_DIR a small fixed bbox is used.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn basemap_download_bench() {
        let max_zoom: u8 = std::env::var("BASEMAP_BENCH_ZOOM").ok().and_then(|z| z.parse().ok()).unwrap_or(12);
        let bbox = match std::env::var("BASEMAP_BENCH_DIR") {
            Ok(dir) => bbox_of_folder(&dir),
            Err(_) => BasemapBbox { min_lon: 18.74, min_lat: 49.55, max_lon: 18.76, max_lat: 49.57 },
        };
        let tiles = tile_math::tiles_for_bbox(&bbox, max_zoom).unwrap();
        println!("bbox={bbox:?} max_zoom={max_zoom} tiles={}", tiles.len());

        let client = Arc::new(build_client().unwrap());
        let key = Arc::new(api_key().expect("PROTOMAP_KEY"));
        let started = std::time::Instant::now();

        let sem = Arc::new(Semaphore::new(FETCH_CONCURRENCY));
        let mut set = tokio::task::JoinSet::new();
        for tile in tiles {
            let (client, key, sem) = (client.clone(), key.clone(), sem.clone());
            set.spawn(async move {
                let _p = sem.acquire_owned().await.unwrap();
                let t = std::time::Instant::now();
                let r = fetch_tile(&client, &key, tile).await;
                (tile, r, t.elapsed())
            });
        }
        let mut fetched = Vec::new();
        let (mut missing, mut failed, mut bytes, mut slowest) = (0, 0, 0usize, Duration::ZERO);
        while let Some(r) = set.join_next().await {
            let (tile, r, elapsed) = r.unwrap();
            slowest = slowest.max(elapsed);
            match r {
                Ok(Some(data)) => {
                    bytes += data.len();
                    fetched.push((tile, data));
                }
                Ok(None) => missing += 1,
                Err(TileError::Fatal(msg)) => panic!("{msg}"),
                Err(TileError::Transient) => failed += 1,
            }
        }
        println!(
            "fetch: total={:?} slowest_tile={slowest:?} ok={} missing={missing} failed={failed} raw={:.2}MB",
            started.elapsed(),
            fetched.len(),
            bytes as f64 / 1e6
        );
        assert_eq!(failed, 0, "{failed} tiles failed");
        for (tile, data) in &fetched {
            assert!(
                !(data.len() >= 2 && data[0] == 0x1f && data[1] == 0x8b),
                "tile {}/{}/{} came back gzip-compressed - would end up double-gzipped in the archive",
                tile.z,
                tile.x,
                tile.y
            );
        }

        let tmp = tempfile::tempdir().expect("tempdir");
        let out_path = tmp.path().join("bench.pmtiles");
        let mut writer = PmTilesWriter::new(TileType::Mvt)
            .max_zoom(max_zoom)
            .bounds(bbox.min_lon, bbox.min_lat, bbox.max_lon, bbox.max_lat)
            .create(std::fs::File::create(&out_path).unwrap())
            .unwrap();
        for (tile, data) in &fetched {
            writer.add_tile(TileCoord::new(tile.z, tile.x, tile.y).unwrap(), data).unwrap();
        }
        writer.finalize().unwrap();
        println!("archive: {:.2}MB", std::fs::metadata(&out_path).unwrap().len() as f64 / 1e6);
    }
}
