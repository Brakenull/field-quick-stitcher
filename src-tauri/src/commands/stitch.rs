use std::path::{Path, PathBuf};

use tauri::{AppHandle, Emitter, State};

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

    let output_path = PathBuf::from(output_path);
    tauri::async_runtime::spawn_blocking(move || {
        pipeline::run(photos, Path::new(&output_path), move |progress| {
            let _ = app.emit("stitch_progress", progress);
        })
    })
    .await
    .map_err(|e| e.to_string())?
}
