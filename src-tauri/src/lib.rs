mod commands;
mod core;
mod models;
#[cfg(test)]
mod pipeline_test;
mod utils;

use std::sync::Mutex;

use commands::export::export_report;
use commands::inspect::inspect_directory;
use models::inspection_result::InspectionResult;

/// Shared backend state, managed by Tauri and injected into commands via `State<AppState>`.
pub struct AppState {
    /// Result of the most recent `inspect_directory` scan, kept around so
    /// `export_report` can write it out without re-sending the whole payload over IPC.
    pub last_inspection: Mutex<Option<InspectionResult>>,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .manage(AppState {
            last_inspection: Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![inspect_directory, export_report])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
