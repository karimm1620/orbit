use std::sync::Arc;

use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

use crate::{
    error::OrbitError,
    repository::{RepositoryRegistry, RepositorySnapshot},
};

#[tauri::command]
pub async fn select_repository(
    app: AppHandle,
    repositories: State<'_, Arc<RepositoryRegistry>>,
) -> Result<Option<RepositorySnapshot>, OrbitError> {
    let selection = app
        .dialog()
        .file()
        .set_title("Open Git Repository")
        .blocking_pick_folder();
    let Some(selection) = selection else {
        return Ok(None);
    };
    let path = selection.into_path().map_err(|_| {
        OrbitError::unsupported(
            "open_repository",
            "The selected location is not a local filesystem path.",
        )
    })?;
    let repositories = Arc::clone(&repositories);

    tauri::async_runtime::spawn_blocking(move || repositories.open(path))
        .await
        .map_err(|_| {
            OrbitError::internal(
                "open_repository",
                "The repository reader stopped unexpectedly.",
            )
        })?
        .map(Some)
}

#[tauri::command]
pub async fn get_repository_snapshot(
    repository_id: String,
    repositories: State<'_, Arc<RepositoryRegistry>>,
) -> Result<RepositorySnapshot, OrbitError> {
    let repositories = Arc::clone(&repositories);

    tauri::async_runtime::spawn_blocking(move || repositories.snapshot(&repository_id))
        .await
        .map_err(|_| {
            OrbitError::internal(
                "read_repository",
                "The repository reader stopped unexpectedly.",
            )
        })?
}
