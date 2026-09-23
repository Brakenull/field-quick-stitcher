mod commands;
mod core;
mod models;
#[cfg(test)]
mod pipeline_test;
#[cfg(test)]
mod real_data_bench;
#[cfg(test)]
mod stitch_pipeline_test;
mod utils;

use std::sync::Mutex;

use commands::export::export_report;
use commands::inspect::inspect_directory;
use commands::offline_basemap::{download_offline_basemap, get_offline_basemap_info};
use commands::stitch::quick_stitch;
use models::inspection_result::InspectionResult;

/// Shared backend state, managed by Tauri and injected into commands via `State<AppState>`.
pub struct AppState {
    /// Result of the most recent `inspect_directory` scan, kept around so
    /// `export_report` can write it out without re-sending the whole payload over IPC.
    pub last_inspection: Mutex<Option<InspectionResult>>,
    /// Serializes `download_offline_basemap` calls - it writes to a single
    /// fixed tmp/final filename (REPLACE semantics: one basemap at a time),
    /// so two overlapping downloads race on the same path and one's rename
    /// fails because the other already moved it away. Held only for the
    /// duration of one download; a second caller gets a clear "already in
    /// progress" error instead of that race.
    pub offline_basemap_download_lock: tokio::sync::Mutex<()>,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .manage(AppState {
            last_inspection: Mutex::new(None),
            offline_basemap_download_lock: tokio::sync::Mutex::new(()),
        })
        .invoke_handler(tauri::generate_handler![
            inspect_directory,
            export_report,
            quick_stitch,
            download_offline_basemap,
            get_offline_basemap_info,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
