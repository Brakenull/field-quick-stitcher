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

use pmtiles::{AsyncPmTilesReader, HashMapCache, HttpBackend, PmTilesWriter, TileCoord, TileType};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{mpsc, Semaphore};

use crate::core::tile_math::{self, TileXY};
use crate::models::offline_basemap::{BasemapBbox, OfflineBasemapInfo, OfflineBasemapProgress, OfflineBasemapStage};
use crate::AppState;

/// 32, not higher: benchmarked on a real survey bbox (`basemap_download_bench`)
/// at z15, 12 -> 32 cut the fetch stage ~30%, while 64 gained nothing more.
const FETCH_CONCURRENCY: usize = 32;
/// Retries for opening the remote reader - a genuinely slow operation (see
/// the reader-open comment below), so few attempts with a generous timeout.
const OPEN_RETRIES: u32 = 3;
/// Stall detector: a request that receives *no bytes at all* for this long
/// is dropped and retried. Deliberately an idle timeout, not a total one -
/// build.protomaps.com's throughput swings wildly (measured 4KB/s-230KB/s
/// minutes apart, on a connection doing 6MB/s elsewhere), and a short total
/// timeout (an earlier 12s one) cancelled slow-but-progressing requests,
/// threw their partial bytes away and restarted them, so on a slow spell
/// tiles never finished at all and got dropped from the archive.
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

async fn find_latest_build(client: &reqwest::Client) -> Result<(String, String), String> {
    let today = chrono::Utc::now().date_naive();
    // Yesterday first: today's build usually isn't published yet, so probing
    // it first mostly just costs one failed HEAD round-trip. Today is still
    // tried right after, in case yesterday's is somehow missing.
    for days_ago in [1, 0, 2, 3, 4, 5, 6, 7, 8, 9] {
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

fn build_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .read_timeout(READ_IDLE_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|e| format!("failed to build HTTP client: {e}"))
}

type RemoteReader = AsyncPmTilesReader<HttpBackend, HashMapCache>;

async fn open_reader(client: &reqwest::Client, url: &str) -> pmtiles::PmtResult<RemoteReader> {
    AsyncPmTilesReader::new_with_cached_url(HashMapCache::default(), client.clone(), url).await
}

/// Exponential backoff between tile retries: 250ms, 500ms, 1s, 2s, 4s.
fn retry_backoff(attempt: u32) -> Duration {
    Duration::from_millis(250 << attempt.min(4))
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

    // Timeouts matter here specifically: with none, a single hung request
    // blocks indefinitely instead of erroring into a retry loop - confirmed
    // empirically (a 4-tile test fetch took ~8 minutes with one stalled
    // request before this was added). See READ_IDLE_TIMEOUT for why the
    // guard is an idle timeout rather than a tight total one.
    let client = build_client()?;
    let (build_url, source_build) = find_latest_build(&client).await?;

    // Opening the reader means range-reading a header/directory out of a
    // 100+GB remote object - confirmed empirically to occasionally exceed
    // even a 20s per-request timeout on a perfectly normal connection (not a
    // hang), so this gets the same retry treatment as an individual tile
    // fetch below, rather than failing the whole download on one slow request.
    //
    // HashMapCache, not the default NoCache: with NoCache every single tile
    // read re-downloads (and re-decompresses) the leaf directory it lives in
    // before fetching the tile itself - two round-trips per tile instead of
    // one, even though neighboring tiles share the same few leaf directories.
    let mut reader_result = open_reader(&client, &build_url).await;
    for attempt in 0..OPEN_RETRIES {
        if reader_result.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500 * (attempt as u64 + 1))).await;
        reader_result = open_reader(&client, &build_url).await;
    }
    let reader = Arc::new(reader_result.map_err(|e| format!("failed to open remote basemap reader at {build_url}: {e}"))?);

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
            for attempt in 0..=TILE_RETRIES {
                // get_tile_decompressed, not get_tile: the remote Protomaps
                // build stores tiles gzip-compressed, and get_tile returns
                // those raw compressed bytes as-is. PmTilesWriter::new(Mvt)
                // defaults to gzip-compressing whatever it's given, so
                // passing the still-compressed bytes through unchanged
                // double-gzips them - the header ends up declaring a single
                // gzip layer while the actual data has two, so every reader
                // that decompresses once per the header gets back garbage
                // and silently renders zero features (confirmed: the
                // resulting archive "downloads fine" but every consumer -
                // FlightMap's old live-swap and OfflineBasemapPreview alike -
                // showed only the base style's background, no map data).
                match reader.get_tile_decompressed(coord).await {
                    Ok(Some(bytes)) => {
                        let _ = tx.send((tile, bytes.to_vec())).await;
                        return;
                    }
                    Ok(None) => return, // tile legitimately absent from the source archive
                    _ if attempt < TILE_RETRIES => {
                        tokio::time::sleep(retry_backoff(attempt)).await;
                    }
                    _ => {
                        failed.fetch_add(1, Ordering::Relaxed);
                    }
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
        source_build,
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
            match reader.get_tile_decompressed(coord).await {
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

    async fn fetch_all<C: pmtiles::DirectoryCache + Sync + Send + 'static>(
        reader: Arc<AsyncPmTilesReader<pmtiles::HttpBackend, C>>,
        tiles: &[TileXY],
        concurrency: usize,
    ) -> (u32, u32, u64) {
        let sem = Arc::new(Semaphore::new(concurrency));
        let mut set = tokio::task::JoinSet::new();
        for &tile in tiles {
            let reader = reader.clone();
            let sem = sem.clone();
            set.spawn(async move {
                let _p = sem.acquire_owned().await.unwrap();
                let coord = TileCoord::new(tile.z, tile.x, tile.y).unwrap();
                let mut outcome = (0u32, 1u32, 0u64);
                let mut attempts = 0;
                for attempt in 0..=TILE_RETRIES {
                    attempts = attempt + 1;
                    let t = std::time::Instant::now();
                    let r = reader.get_tile_decompressed(coord).await;
                    let kind = if r.is_ok() { "ok" } else { "err" };
                    if std::env::var("BASEMAP_BENCH_VERBOSE").is_ok() {
                        println!("    tile {}/{}/{} attempt {attempts}: {kind} in {:?}", tile.z, tile.x, tile.y, t.elapsed());
                    }
                    match r {
                        Ok(Some(b)) => { outcome = (1, 0, b.len() as u64); break; }
                        Ok(None) => { outcome = (0, 0, 0); break; }
                        _ if attempt < TILE_RETRIES => tokio::time::sleep(retry_backoff(attempt)).await,
                        _ => {}
                    }
                }
                outcome
            });
        }
        let (mut ok, mut failed, mut bytes) = (0, 0, 0);
        while let Some(r) = set.join_next().await {
            let (o, f, b) = r.unwrap();
            ok += o;
            failed += f;
            bytes += b;
        }
        (ok, failed, bytes)
    }

    /// Times the download stages for a real dataset's bbox. Run with:
    ///   BASEMAP_BENCH_DIR=<photo folder> cargo test basemap_download_bench -- --ignored --nocapture
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn basemap_download_bench() {
        let dir = std::env::var("BASEMAP_BENCH_DIR").expect("set BASEMAP_BENCH_DIR");
        let max_zoom: u8 = std::env::var("BASEMAP_BENCH_ZOOM").ok().and_then(|z| z.parse().ok()).unwrap_or(12);
        let bbox = bbox_of_folder(&dir);
        let tiles = tile_math::tiles_for_bbox(&bbox, max_zoom).unwrap();
        println!("bbox={bbox:?} max_zoom={max_zoom} tiles={}", tiles.len());

        let client = build_client().unwrap();
        let t = std::time::Instant::now();
        let (url, build) = find_latest_build(&client).await.unwrap();
        println!("find_latest_build: {:?} ({build})", t.elapsed());

        // Baseline = the pre-optimization setup (no directory cache, 12
        // concurrent); the rest use the production reader (HashMapCache).
        let variants: &[(&str, bool, usize)] =
            &[("NoCache  c=12 (old)", false, 12), ("HashMap  c=12", true, 12), ("HashMap  c=32 (current)", true, FETCH_CONCURRENCY)];
        for &(name, cached, conc) in variants {
            if std::env::var("BASEMAP_BENCH_VERBOSE").is_ok() && !name.contains("current") {
                continue;
            }
            let t = std::time::Instant::now();
            let result = if cached {
                match open_reader(&client, &url).await {
                    Ok(r) => {
                        print!("[{name}] open={:?} ", t.elapsed());
                        Ok(fetch_all(Arc::new(r), &tiles, conc).await)
                    }
                    Err(e) => Err(e),
                }
            } else {
                match AsyncPmTilesReader::new_with_url(client.clone(), &url).await {
                    Ok(r) => {
                        print!("[{name}] open={:?} ", t.elapsed());
                        Ok(fetch_all(Arc::new(r), &tiles, conc).await)
                    }
                    Err(e) => Err(e),
                }
            };
            match result {
                Ok((ok, failed, bytes)) => println!(
                    "total={:?} ok={ok} failed={failed} decompressed={:.1}MB",
                    t.elapsed(),
                    bytes as f64 / 1e6
                ),
                Err(e) => println!("[{name}] reader open failed after {:?}: {e}", t.elapsed()),
            }
        }
    }
}
