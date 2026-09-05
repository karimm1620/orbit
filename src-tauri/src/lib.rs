mod commands;
mod error;
mod git;
mod repository;

use std::sync::Arc;

use repository::RepositoryRegistry;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(Arc::new(RepositoryRegistry::default()))
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            commands::select_repository,
            commands::get_repository_snapshot
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
