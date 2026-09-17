use std::sync::Arc;

use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

use crate::{
    change_sets::RepositoryChanges,
    error::OrbitError,
    git::{DiffSide, FileDiff},
    history_sessions::CommitHistoryPage,
    mutation_registry::MutationReceipt,
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
pub async fn get_file_diff(
    repository_id: String,
    change_set_id: String,
    file_id: String,
    side: DiffSide,
    repositories: State<'_, Arc<RepositoryRegistry>>,
) -> Result<FileDiff, OrbitError> {
    let repositories = Arc::clone(&repositories);

    tauri::async_runtime::spawn_blocking(move || {
        repositories.file_diff(&repository_id, &change_set_id, &file_id, side)
    })
    .await
    .map_err(|_| {
        OrbitError::internal(
            "read_file_diff",
            "The selected-file diff reader stopped unexpectedly.",
        )
    })?
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

#[tauri::command]
pub async fn get_repository_changes(
    repository_id: String,
    repositories: State<'_, Arc<RepositoryRegistry>>,
) -> Result<RepositoryChanges, OrbitError> {
    let repositories = Arc::clone(&repositories);

    tauri::async_runtime::spawn_blocking(move || repositories.repository_changes(&repository_id))
        .await
        .map_err(|_| {
            OrbitError::internal(
                "read_repository_changes",
                "The repository changes reader stopped unexpectedly.",
            )
        })?
}

#[tauri::command]
pub async fn get_commit_history_page(
    repository_id: String,
    cursor: Option<String>,
    page_size: Option<i64>,
    repositories: State<'_, Arc<RepositoryRegistry>>,
) -> Result<CommitHistoryPage, OrbitError> {
    let repositories = Arc::clone(&repositories);

    tauri::async_runtime::spawn_blocking(move || {
        repositories.commit_history_page(&repository_id, cursor.as_deref(), page_size)
    })
    .await
    .map_err(|_| {
        OrbitError::internal(
            "read_commit_history",
            "The commit-history reader stopped unexpectedly.",
        )
    })?
}

#[tauri::command]
pub async fn stage_file(
    repository_id: String,
    change_set_id: String,
    file_id: String,
    repositories: State<'_, Arc<RepositoryRegistry>>,
) -> Result<MutationReceipt, OrbitError> {
    let repositories = Arc::clone(&repositories);
    tauri::async_runtime::spawn_blocking(move || {
        repositories.stage_file(&repository_id, &change_set_id, &file_id)
    })
    .await
    .map_err(|_| mutation_worker_error("stage_file"))?
}

#[tauri::command]
pub async fn unstage_file(
    repository_id: String,
    change_set_id: String,
    file_id: String,
    repositories: State<'_, Arc<RepositoryRegistry>>,
) -> Result<MutationReceipt, OrbitError> {
    let repositories = Arc::clone(&repositories);
    tauri::async_runtime::spawn_blocking(move || {
        repositories.unstage_file(&repository_id, &change_set_id, &file_id)
    })
    .await
    .map_err(|_| mutation_worker_error("unstage_file"))?
}

#[tauri::command]
pub async fn stage_all(
    repository_id: String,
    change_set_id: String,
    repositories: State<'_, Arc<RepositoryRegistry>>,
) -> Result<MutationReceipt, OrbitError> {
    let repositories = Arc::clone(&repositories);
    tauri::async_runtime::spawn_blocking(move || {
        repositories.stage_all(&repository_id, &change_set_id)
    })
    .await
    .map_err(|_| mutation_worker_error("stage_all"))?
}

#[tauri::command]
pub async fn unstage_all(
    repository_id: String,
    change_set_id: String,
    repositories: State<'_, Arc<RepositoryRegistry>>,
) -> Result<MutationReceipt, OrbitError> {
    let repositories = Arc::clone(&repositories);
    tauri::async_runtime::spawn_blocking(move || {
        repositories.unstage_all(&repository_id, &change_set_id)
    })
    .await
    .map_err(|_| mutation_worker_error("unstage_all"))?
}

#[tauri::command]
pub async fn create_commit(
    repository_id: String,
    change_set_id: String,
    message: String,
    repositories: State<'_, Arc<RepositoryRegistry>>,
) -> Result<MutationReceipt, OrbitError> {
    let repositories = Arc::clone(&repositories);
    tauri::async_runtime::spawn_blocking(move || {
        repositories.create_commit(&repository_id, &change_set_id, &message)
    })
    .await
    .map_err(|_| mutation_worker_error("create_commit"))?
}

fn mutation_worker_error(operation: &'static str) -> OrbitError {
    OrbitError::internal(
        operation,
        "The repository mutation worker stopped unexpectedly.",
    )
}
