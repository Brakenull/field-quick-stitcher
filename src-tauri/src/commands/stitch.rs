use std::io::Write;
use std::path::{Path, PathBuf};

use tauri::{AppHandle, Emitter, Manager, State};

use crate::core::stitching::pipeline;
use crate::models::photo_meta::PhotoMeta;
use crate::models::stitch_result::StitchResult;
use crate::AppState;

/// Builds a quick orthomosaic GeoTIFF from the photos of the most recent
/// `inspect_directory` scan (spec section 5) - only photos with a full
/// footprint (GPS + yaw + altitude + focal + sensor) can be placed, so this
/// reuses exactly the same filtering `inspect_directory` already applied for
/// the overlap heatmap.
#[tauri::command]
pub async fn quick_stitch(app: AppHandle, state: State<'_, AppState>, output_path: String) -> Result<StitchResult, String> {
    let photos: Vec<PhotoMeta> = {
        let guard = state.last_inspection.lock().map_err(|e| e.to_string())?;
        let result = guard.as_ref().ok_or_else(|| "No scan results available. Run Inspect first.".to_string())?;
        result.photos.iter().filter(|p| p.footprint.is_some()).cloned().collect()
    };

    let extractor_model_path = app
        .path()
        .resolve("resources/superpoint.onnx", tauri::path::BaseDirectory::Resource)
        .map_err(|e| e.to_string())?;
    let matcher_model_path = app
        .path()
        .resolve("resources/superpoint_lightglue.trt.onnx", tauri::path::BaseDirectory::Resource)
        .map_err(|e| e.to_string())?;

    let output_path = PathBuf::from(output_path);
    let app_for_scope = app.clone();

    // TEMPORARY DIAGNOSTIC: mirrors every progress tick to a log file
    // (independent of the `stitch_progress` Tauri event/UI) so progress can
    // be confirmed even if the frontend's event listener stops updating -
    // see conversation history for why (WebView2 renderer discard under
    // memory pressure is the leading theory). Remove once confirmed.
    let log_path = std::env::temp_dir().join("quick_stitch_progress.log");
    let _ = std::fs::write(&log_path, format!("=== quick_stitch started - log at {} ===\n", log_path.display()));
    let log_start = std::time::Instant::now();
    let log_path_for_closure = log_path.clone();

    let result = tauri::async_runtime::spawn_blocking(move || {
        pipeline::run(photos, Path::new(&output_path), &extractor_model_path, &matcher_model_path, move |progress| {
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&log_path_for_closure) {
                let _ = writeln!(f, "{:>8.1}s  {:?}: {}%", log_start.elapsed().as_secs_f64(), progress.stage, progress.percent);
            }
            let _ = app.emit("stitch_progress", progress);
        })
    })
    .await
    .map_err(|e| e.to_string())??;

    // The preview PNG lives wherever the user chose to save the GeoTIFF (via
    // the save dialog), so it can't be covered by a static scope in
    // tauri.conf.json - grant the asset protocol access to this one file now
    // that it exists, so FlightMap's `convertFileSrc(previewPath)` can load it.
    app_for_scope.asset_protocol_scope().allow_file(&result.preview_path).map_err(|e| e.to_string())?;

    Ok(result)
}
