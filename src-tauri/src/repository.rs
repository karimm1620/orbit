use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        RwLock,
    },
};

use serde::Serialize;

use crate::{
    change_sets::{AuthorizedChangeSet, AuthorizedFile, ChangeSetRegistry, RepositoryChanges},
    error::OrbitError,
    git::{
        ensure_mutation_state_allowed, index_lock_exists, read_detailed_status, read_file_diff,
        read_recent_commits, read_status, run_commit, run_stage_all, run_stage_file,
        run_unstage_all, run_unstage_file, ChangeKind, CommitSummary, DiffSelection, DiffSide,
        FileDiff, GitMutationOutput, GitMutationTermination, GitRunner, HeadSnapshot, StatusEntry,
        StatusSnapshot, WorkingTreeSnapshot,
    },
    history_sessions::{CommitHistoryPage, CommitHistoryRegistry},
    mutation_registry::{MutationOperation, MutationOutcome, MutationReceipt, MutationRegistry},
};

const REPOSITORY_PROBE_LIMIT: usize = 16 * 1024;
const MAX_COMMIT_MESSAGE_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositorySnapshot {
    pub repository_id: String,
    pub root: String,
    pub display_name: String,
    pub head: HeadSnapshot,
    pub working_tree: WorkingTreeSnapshot,
    pub recent_commits: Vec<CommitSummary>,
}

pub struct RepositoryRegistry {
    roots: RwLock<HashMap<String, PathBuf>>,
    next_id: AtomicU64,
    runner: GitRunner,
    histories: CommitHistoryRegistry,
    changes: ChangeSetRegistry,
    mutations: MutationRegistry,
}

impl Default for RepositoryRegistry {
    fn default() -> Self {
        Self {
            roots: RwLock::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            runner: GitRunner::default(),
            histories: CommitHistoryRegistry::default(),
            changes: ChangeSetRegistry::default(),
            mutations: MutationRegistry::default(),
        }
    }
}

impl RepositoryRegistry {
    #[cfg(test)]
    fn with_runner(runner: GitRunner) -> Self {
        Self {
            runner,
            ..Self::default()
        }
    }

    pub fn open(&self, selected: PathBuf) -> Result<RepositorySnapshot, OrbitError> {
        self.runner.version()?;
        let root = resolve_repository(&self.runner, &selected)?;
        let repository_id = self.next_repository_id();
        let snapshot = build_snapshot(&self.runner, repository_id.clone(), &root)?;

        self.roots
            .write()
            .map_err(|_| {
                OrbitError::internal("open_repository", "Repository state is unavailable.")
            })?
            .insert(repository_id, root);

        Ok(snapshot)
    }

    pub fn snapshot(&self, repository_id: &str) -> Result<RepositorySnapshot, OrbitError> {
        let root = self.authorized_root(repository_id)?;
        build_snapshot(&self.runner, repository_id.to_owned(), &root)
    }

    pub fn commit_history_page(
        &self,
        repository_id: &str,
        cursor: Option<&str>,
        page_size: Option<i64>,
    ) -> Result<CommitHistoryPage, OrbitError> {
        let root = self.authorized_root(repository_id)?;
        self.runner.require_no_lazy_fetch()?;
        if let Some(cursor) = cursor {
            self.histories
                .load_more(&self.runner, repository_id, &root, cursor, page_size)
        } else {
            let status = read_status(&self.runner, &root)?;
            self.histories
                .start(&self.runner, repository_id, &root, &status.head, page_size)
        }
    }

    pub fn repository_changes(&self, repository_id: &str) -> Result<RepositoryChanges, OrbitError> {
        // Reserve order before repository validation can block on Git, so worker
        // scheduling cannot make an older request authoritative over a newer one.
        let generation = self.changes.reserve_refresh();
        let root = self.registered_root(repository_id)?;
        self.changes
            .begin_refresh(repository_id, &root, generation)?;
        self.revalidate_root(repository_id, &root)?;
        let status = read_detailed_status(&self.runner, &root)?;
        self.changes.install(
            repository_id,
            &root,
            generation,
            status.head,
            status.working_tree,
            status.changes,
        )
    }

    pub fn file_diff(
        &self,
        repository_id: &str,
        change_set_id: &str,
        file_id: &str,
        side: DiffSide,
    ) -> Result<FileDiff, OrbitError> {
        let root = self.authorized_root(repository_id)?;
        let file = self
            .changes
            .authorize_file(repository_id, &root, change_set_id, file_id)?;
        let status = read_detailed_status(&self.runner, &root)?;
        read_file_diff(
            &self.runner,
            &root,
            &diff_selection(file),
            &status.changes,
            change_set_id,
            file_id,
            side,
        )
    }

    pub fn stage_file(
        &self,
        repository_id: &str,
        change_set_id: &str,
        file_id: &str,
    ) -> Result<MutationReceipt, OrbitError> {
        self.mutate_staging(
            repository_id,
            change_set_id,
            StagingRequest::StageFile { file_id },
        )
    }

    pub fn unstage_file(
        &self,
        repository_id: &str,
        change_set_id: &str,
        file_id: &str,
    ) -> Result<MutationReceipt, OrbitError> {
        self.mutate_staging(
            repository_id,
            change_set_id,
            StagingRequest::UnstageFile { file_id },
        )
    }

    pub fn stage_all(
        &self,
        repository_id: &str,
        change_set_id: &str,
    ) -> Result<MutationReceipt, OrbitError> {
        self.mutate_staging(repository_id, change_set_id, StagingRequest::StageAll)
    }

    pub fn unstage_all(
        &self,
        repository_id: &str,
        change_set_id: &str,
    ) -> Result<MutationReceipt, OrbitError> {
        self.mutate_staging(repository_id, change_set_id, StagingRequest::UnstageAll)
    }

    pub fn create_commit(
        &self,
        repository_id: &str,
        change_set_id: &str,
        message: &str,
    ) -> Result<MutationReceipt, OrbitError> {
        validate_commit_message(message)?;
        let generation = self.changes.reserve_refresh();
        let root = self.authorized_root(repository_id)?;
        self.runner.require_no_lazy_fetch()?;
        let _lease = self.mutations.acquire(repository_id)?;
        let authority = self
            .changes
            .authorize_change_set(repository_id, &root, change_set_id)?;
        let pre_status = read_detailed_status(&self.runner, &root)?;
        ensure_mutation_state_allowed(&self.runner, &root, &pre_status)?;
        ensure_change_set_compatible(&authority, &pre_status)?;
        if pre_status.working_tree.staged == 0 {
            return Err(OrbitError::commit_has_no_staged_changes());
        }

        self.changes
            .begin_mutation(repository_id, &root, change_set_id, generation)?;
        // Fence captured history before starting a commit: once Git starts,
        // failed post-status cannot prove that the old HEAD is still current.
        self.histories.invalidate_repository(repository_id)?;
        let output = run_commit(&self.runner, &root, message.as_bytes())?;
        let post_status = read_detailed_status(&self.runner, &root);
        let lock_remains = index_lock_exists(&self.runner, &root);
        self.finish_commit_mutation(
            repository_id,
            &root,
            generation,
            pre_status,
            post_status,
            output,
            lock_remains,
        )
    }

    fn mutate_staging(
        &self,
        repository_id: &str,
        change_set_id: &str,
        request: StagingRequest<'_>,
    ) -> Result<MutationReceipt, OrbitError> {
        // Reserve before blocking repository work so any older detailed-status
        // worker has a lower generation than this mutation intent.
        let generation = self.changes.reserve_refresh();
        let root = self.authorized_root(repository_id)?;
        self.runner.require_no_lazy_fetch()?;
        let _lease = self.mutations.acquire(repository_id)?;

        let authority = match request.file_id() {
            Some(file_id) => {
                let change_set =
                    self.changes
                        .authorize_change_set(repository_id, &root, change_set_id)?;
                MutationAuthority::File {
                    file: self.changes.authorize_file(
                        repository_id,
                        &root,
                        change_set_id,
                        file_id,
                    )?,
                    head: change_set.head,
                }
            }
            None => MutationAuthority::All(self.changes.authorize_change_set(
                repository_id,
                &root,
                change_set_id,
            )?),
        };
        let pre_status = read_detailed_status(&self.runner, &root)?;
        ensure_mutation_state_allowed(&self.runner, &root, &pre_status)?;
        let prepared = prepare_mutation(request, &authority, &pre_status)?;

        // This is the point of no return for handle authority. It atomically
        // fences older refresh generations and retires every old file handle.
        self.changes
            .begin_mutation(repository_id, &root, change_set_id, generation)?;

        let output = prepared.run(&self.runner, &root)?;
        let post_status = read_detailed_status(&self.runner, &root);
        let lock_remains = index_lock_exists(&self.runner, &root);
        self.finish_staging_mutation(
            repository_id,
            &root,
            generation,
            prepared,
            pre_status,
            post_status,
            output,
            lock_remains,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_staging_mutation(
        &self,
        repository_id: &str,
        root: &Path,
        generation: crate::change_sets::ChangesRefreshGeneration,
        prepared: PreparedMutation,
        pre_status: StatusSnapshot,
        post_status: Result<StatusSnapshot, OrbitError>,
        output: GitMutationOutput,
        lock_remains: bool,
    ) -> Result<MutationReceipt, OrbitError> {
        let operation = prepared.operation;
        let post = post_status.ok();
        let state_changed = post.as_ref().is_some_and(|post| post != &pre_status);
        let expected_transition = post
            .as_ref()
            .is_some_and(|post| prepared.expected_transition(post));
        let exited_successfully = output.termination == GitMutationTermination::Exited
            && output
                .status
                .as_ref()
                .is_some_and(|status| status.success());
        let exited_with_rejection = output.termination == GitMutationTermination::Exited
            && output
                .status
                .as_ref()
                .is_some_and(|status| !status.success());

        let outcome = if exited_successfully && expected_transition {
            MutationOutcome::Applied
        } else if exited_with_rejection && !state_changed {
            MutationOutcome::Rejected
        } else {
            MutationOutcome::Uncertain
        };

        let head_changed = post
            .as_ref()
            .is_some_and(|post| post.head != pre_status.head);
        if head_changed {
            self.histories.invalidate_repository(repository_id)?;
        }

        let repository_changes = post.and_then(|post| {
            self.changes
                .install(
                    repository_id,
                    root,
                    generation,
                    post.head,
                    post.working_tree,
                    post.changes,
                )
                .ok()
        });
        let refresh_required = repository_changes.is_none();
        let issue = mutation_issue(
            operation,
            outcome,
            output.termination,
            &output.stderr,
            lock_remains,
            refresh_required,
        );

        Ok(MutationReceipt {
            operation,
            outcome,
            issue,
            repository_changes,
            commit_oid: None,
            refresh_required,
            head_changed,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_commit_mutation(
        &self,
        repository_id: &str,
        root: &Path,
        generation: crate::change_sets::ChangesRefreshGeneration,
        pre_status: StatusSnapshot,
        post_status: Result<StatusSnapshot, OrbitError>,
        output: GitMutationOutput,
        lock_remains: bool,
    ) -> Result<MutationReceipt, OrbitError> {
        let post = post_status.ok();
        let head_changed = post
            .as_ref()
            .is_some_and(|post| post.head != pre_status.head);
        let exited_successfully = output.termination == GitMutationTermination::Exited
            && output
                .status
                .as_ref()
                .is_some_and(|status| status.success());
        let exited_with_rejection = output.termination == GitMutationTermination::Exited
            && output
                .status
                .as_ref()
                .is_some_and(|status| !status.success());
        let outcome = if head_changed {
            MutationOutcome::Applied
        } else if exited_successfully && post.is_some() {
            MutationOutcome::Uncertain
        } else if exited_with_rejection && post.is_some() {
            MutationOutcome::Rejected
        } else {
            MutationOutcome::Uncertain
        };
        let commit_oid = post.as_ref().and_then(|post| {
            if head_changed {
                post.head.oid.clone()
            } else {
                None
            }
        });
        let repository_changes = post.and_then(|post| {
            self.changes
                .install(
                    repository_id,
                    root,
                    generation,
                    post.head,
                    post.working_tree,
                    post.changes,
                )
                .ok()
        });
        let refresh_required = repository_changes.is_none();
        let issue = mutation_issue(
            MutationOperation::CreateCommit,
            outcome,
            output.termination,
            &output.stderr,
            lock_remains,
            refresh_required,
        );
        Ok(MutationReceipt {
            operation: MutationOperation::CreateCommit,
            outcome,
            issue,
            repository_changes,
            commit_oid,
            refresh_required,
            head_changed,
        })
    }

    fn authorized_root(&self, repository_id: &str) -> Result<PathBuf, OrbitError> {
        let root = self.registered_root(repository_id)?;
        self.revalidate_root(repository_id, &root)?;
        Ok(root)
    }

    fn registered_root(&self, repository_id: &str) -> Result<PathBuf, OrbitError> {
        if !valid_repository_id(repository_id) {
            return Err(OrbitError::repository_unavailable(
                "This repository is no longer authorized in the current Orbit session.",
            ));
        }

        self.roots
            .read()
            .map_err(|_| {
                OrbitError::internal("read_repository", "Repository state is unavailable.")
            })?
            .get(repository_id)
            .cloned()
            .ok_or_else(|| {
                OrbitError::repository_unavailable(
                    "This repository is no longer authorized in the current Orbit session.",
                )
            })
    }

    fn revalidate_root(&self, repository_id: &str, root: &Path) -> Result<(), OrbitError> {
        let resolved = match resolve_repository(&self.runner, root) {
            Ok(resolved) => resolved,
            Err(error) => {
                let error = if error.code == "not_a_repository" {
                    OrbitError::repository_unavailable(
                        "The opened directory is no longer an available Git working tree.",
                    )
                } else {
                    error
                };
                if error.code == "repository_unavailable" {
                    self.histories.invalidate_repository(repository_id)?;
                    self.changes.invalidate_repository(repository_id)?;
                }
                return Err(error);
            }
        };

        if resolved != root {
            self.histories.invalidate_repository(repository_id)?;
            self.changes.invalidate_repository(repository_id)?;
            return Err(OrbitError::repository_unavailable(
                "The opened repository now resolves to a different working tree.",
            ));
        }

        Ok(())
    }

    fn next_repository_id(&self) -> String {
        let value = self.next_id.fetch_add(1, Ordering::Relaxed);
        format!("repository-{value:016x}")
    }
}

enum StagingRequest<'a> {
    StageFile { file_id: &'a str },
    UnstageFile { file_id: &'a str },
    StageAll,
    UnstageAll,
}

impl StagingRequest<'_> {
    fn file_id(&self) -> Option<&str> {
        match self {
            Self::StageFile { file_id } | Self::UnstageFile { file_id } => Some(file_id),
            Self::StageAll | Self::UnstageAll => None,
        }
    }
}

enum MutationAuthority {
    File {
        file: AuthorizedFile,
        head: HeadSnapshot,
    },
    All(AuthorizedChangeSet),
}

struct PreparedMutation {
    operation: MutationOperation,
    paths: Vec<Vec<u8>>,
    selected_path: Option<Vec<u8>>,
    expected_kind: Option<ChangeKind>,
    unborn: bool,
}

impl PreparedMutation {
    fn run(&self, runner: &GitRunner, root: &Path) -> Result<GitMutationOutput, OrbitError> {
        match self.operation {
            MutationOperation::StageFile => run_stage_file(runner, root, &self.paths),
            MutationOperation::UnstageFile => {
                run_unstage_file(runner, root, &self.paths, self.unborn)
            }
            MutationOperation::StageAll => run_stage_all(runner, root),
            MutationOperation::UnstageAll => run_unstage_all(runner, root, self.unborn),
            MutationOperation::CreateCommit => Err(OrbitError::internal(
                "mutate_repository",
                "A commit cannot use a staging mutation plan.",
            )),
        }
    }

    fn expected_transition(&self, post: &StatusSnapshot) -> bool {
        match self.operation {
            MutationOperation::StageFile => {
                let Some(path) = self.selected_path.as_ref() else {
                    return false;
                };
                post.changes
                    .iter()
                    .find(|entry| &entry.path == path)
                    .and_then(|entry| entry.staged.as_ref())
                    .is_some_and(|facet| Some(facet.kind) == self.expected_kind)
            }
            MutationOperation::UnstageFile => {
                let Some(path) = self.selected_path.as_ref() else {
                    return false;
                };
                post.changes
                    .iter()
                    .find(|entry| &entry.path == path)
                    .is_none_or(|entry| entry.staged.is_none())
            }
            MutationOperation::StageAll => {
                post.working_tree.unstaged == 0
                    && post.working_tree.untracked == 0
                    && post.working_tree.conflicted == 0
            }
            MutationOperation::UnstageAll => post.working_tree.staged == 0,
            MutationOperation::CreateCommit => false,
        }
    }
}

fn prepare_mutation(
    request: StagingRequest<'_>,
    authority: &MutationAuthority,
    fresh: &StatusSnapshot,
) -> Result<PreparedMutation, OrbitError> {
    match (request, authority) {
        (StagingRequest::StageFile { .. }, MutationAuthority::File { file, head }) => {
            if head != &fresh.head {
                return Err(stale_mutation());
            }
            let fresh_file = compatible_fresh_file(file, fresh)?;
            let facet = file.unstaged.as_ref().ok_or_else(|| {
                OrbitError::mutation_not_applicable(
                    "The selected file no longer has an unstaged change to stage.",
                )
            })?;
            if fresh_file.unstaged.as_ref() != Some(facet) {
                return Err(stale_mutation());
            }
            let expected_kind = if facet.kind == ChangeKind::Untracked {
                ChangeKind::Added
            } else {
                facet.kind
            };
            Ok(PreparedMutation {
                operation: MutationOperation::StageFile,
                paths: selected_paths(file, facet.kind),
                selected_path: Some(file.path.clone()),
                expected_kind: Some(expected_kind),
                unborn: fresh.head.oid.is_none(),
            })
        }
        (StagingRequest::UnstageFile { .. }, MutationAuthority::File { file, head }) => {
            if head != &fresh.head {
                return Err(stale_mutation());
            }
            let fresh_file = compatible_fresh_file(file, fresh)?;
            let facet = file.staged.as_ref().ok_or_else(|| {
                OrbitError::mutation_not_applicable(
                    "The selected file no longer has a staged change to unstage.",
                )
            })?;
            if fresh_file.staged.as_ref() != Some(facet) {
                return Err(stale_mutation());
            }
            let unborn = fresh.head.oid.is_none();
            if unborn && facet.kind != ChangeKind::Added {
                return Err(OrbitError::mutation_not_applicable(
                    "Only a staged addition can be unstaged before the repository has its first commit.",
                ));
            }
            Ok(PreparedMutation {
                operation: MutationOperation::UnstageFile,
                paths: selected_paths(file, facet.kind),
                selected_path: Some(file.path.clone()),
                expected_kind: None,
                unborn,
            })
        }
        (StagingRequest::StageAll, MutationAuthority::All(change_set)) => {
            ensure_change_set_compatible(change_set, fresh)?;
            if fresh.working_tree.unstaged == 0 && fresh.working_tree.untracked == 0 {
                return Err(OrbitError::mutation_not_applicable(
                    "There are no unstaged changes to stage.",
                ));
            }
            Ok(PreparedMutation {
                operation: MutationOperation::StageAll,
                paths: Vec::new(),
                selected_path: None,
                expected_kind: None,
                unborn: fresh.head.oid.is_none(),
            })
        }
        (StagingRequest::UnstageAll, MutationAuthority::All(change_set)) => {
            ensure_change_set_compatible(change_set, fresh)?;
            if fresh.working_tree.staged == 0 {
                return Err(OrbitError::mutation_not_applicable(
                    "There are no staged changes to unstage.",
                ));
            }
            Ok(PreparedMutation {
                operation: MutationOperation::UnstageAll,
                paths: Vec::new(),
                selected_path: None,
                expected_kind: None,
                unborn: fresh.head.oid.is_none(),
            })
        }
        _ => Err(OrbitError::internal(
            "mutate_repository",
            "The staging request did not match its authorization scope.",
        )),
    }
}

fn compatible_fresh_file<'a>(
    selected: &AuthorizedFile,
    fresh: &'a StatusSnapshot,
) -> Result<&'a StatusEntry, OrbitError> {
    let current = fresh
        .changes
        .iter()
        .find(|entry| entry.path == selected.path)
        .ok_or_else(stale_mutation)?;
    if current.original_path != selected.original_path
        || current.conflict != selected.conflict
        || current.submodule != selected.submodule
    {
        return Err(stale_mutation());
    }
    if current.conflict.is_some() {
        return Err(OrbitError::mutation_not_applicable(
            "Conflict resolution is not part of the current staging workflow.",
        ));
    }
    Ok(current)
}

fn ensure_change_set_compatible(
    selected: &AuthorizedChangeSet,
    fresh: &StatusSnapshot,
) -> Result<(), OrbitError> {
    if selected.head != fresh.head || selected.files.len() != fresh.changes.len() {
        return Err(stale_mutation());
    }
    if selected
        .files
        .iter()
        .zip(&fresh.changes)
        .any(|(selected, fresh)| !same_file_state(selected, fresh))
    {
        return Err(stale_mutation());
    }
    Ok(())
}

fn same_file_state(selected: &AuthorizedFile, fresh: &StatusEntry) -> bool {
    selected.path == fresh.path
        && selected.original_path == fresh.original_path
        && selected.staged == fresh.staged
        && selected.unstaged == fresh.unstaged
        && selected.conflict == fresh.conflict
        && selected.submodule == fresh.submodule
}

fn selected_paths(file: &AuthorizedFile, kind: ChangeKind) -> Vec<Vec<u8>> {
    let mut paths = vec![file.path.clone()];
    if kind == ChangeKind::Renamed {
        if let Some(original) = file.original_path.clone() {
            paths.push(original);
        }
    }
    paths
}

fn stale_mutation() -> OrbitError {
    OrbitError::mutation_stale(
        "The repository changed after this change list was loaded. Refresh before trying again.",
    )
}

fn validate_commit_message(message: &str) -> Result<(), OrbitError> {
    if message.len() > MAX_COMMIT_MESSAGE_BYTES
        || message.as_bytes().contains(&0)
        || !message.chars().any(|character| !character.is_whitespace())
    {
        return Err(OrbitError::commit_message_invalid());
    }
    Ok(())
}

fn mutation_issue(
    operation: MutationOperation,
    outcome: MutationOutcome,
    termination: GitMutationTermination,
    stderr: &[u8],
    lock_remains: bool,
    refresh_required: bool,
) -> Option<OrbitError> {
    let operation_name = match operation {
        MutationOperation::StageFile => "stage_file",
        MutationOperation::UnstageFile => "unstage_file",
        MutationOperation::StageAll => "stage_all",
        MutationOperation::UnstageAll => "unstage_all",
        MutationOperation::CreateCommit => "create_commit",
    };
    if outcome == MutationOutcome::Rejected {
        return Some(OrbitError::mutation_rejected(
            operation_name,
            stderr,
            lock_remains,
        ));
    }
    if outcome == MutationOutcome::Uncertain {
        let (code, message) = match termination {
            GitMutationTermination::TimedOut => (
                "mutation_timed_out",
                "Git exceeded Orbit's staging deadline. Repository state was refreshed where possible; do not retry until it is inspected.",
            ),
            GitMutationTermination::OutputTooLarge => (
                "mutation_outcome_uncertain",
                "Git produced more output than Orbit retains. Inspect the refreshed repository state before retrying.",
            ),
            GitMutationTermination::Exited | GitMutationTermination::RunnerFailed => (
                "mutation_outcome_uncertain",
                "Orbit could not prove that the requested index transition completed. Inspect the refreshed repository state before retrying.",
            ),
        };
        return Some(OrbitError::mutation_uncertain(
            code,
            operation_name,
            message,
            stderr,
            lock_remains,
        ));
    }
    if refresh_required {
        return Some(OrbitError::post_mutation_refresh_failed(operation_name));
    }
    if operation == MutationOperation::CreateCommit && termination != GitMutationTermination::Exited
    {
        let mut warning = OrbitError::mutation_warning(operation_name, stderr, lock_remains);
        warning.message = match termination {
            GitMutationTermination::TimedOut => "HEAD advanced, but Git exceeded Orbit's commit deadline. The commit was applied; inspect repository state before continuing.",
            GitMutationTermination::OutputTooLarge => "HEAD advanced, but Git exceeded Orbit's output bound. The commit was applied; inspect repository state before continuing.",
            _ => "HEAD advanced, but Orbit could not complete Git process I/O. The commit was applied; inspect repository state before continuing.",
        }.into();
        return Some(warning);
    }
    if !stderr.is_empty() || lock_remains {
        return Some(OrbitError::mutation_warning(
            operation_name,
            stderr,
            lock_remains,
        ));
    }
    None
}

fn diff_selection(file: AuthorizedFile) -> DiffSelection {
    DiffSelection {
        path: file.path,
        original_path: file.original_path,
        staged: file.staged,
        unstaged: file.unstaged,
        conflict: file.conflict,
        submodule: file.submodule,
    }
}

fn valid_repository_id(repository_id: &str) -> bool {
    repository_id
        .strip_prefix("repository-")
        .is_some_and(|suffix| {
            suffix.len() == 16 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

fn resolve_repository(runner: &GitRunner, selected: &Path) -> Result<PathBuf, OrbitError> {
    let selected = fs::canonicalize(selected).map_err(|_| {
        OrbitError::repository_unavailable(
            "The selected directory does not exist or cannot be read.",
        )
    })?;
    let metadata = fs::metadata(&selected).map_err(|_| {
        OrbitError::repository_unavailable("The selected directory cannot be read.")
    })?;
    if !metadata.is_dir() {
        return Err(OrbitError::not_a_repository());
    }

    let probe = runner.run(
        Some(&selected),
        "validate_repository",
        [
            "--no-optional-locks",
            "rev-parse",
            "--is-inside-work-tree",
            "--is-bare-repository",
        ],
        REPOSITORY_PROBE_LIMIT,
    )?;
    if !probe.status.success() {
        return Err(if has_repository_marker(&selected) {
            OrbitError::git_failed("validate_repository", &probe.stderr)
        } else {
            OrbitError::not_a_repository()
        });
    }
    let probe = std::str::from_utf8(&probe.stdout).map_err(|_| {
        OrbitError::unsupported(
            "validate_repository",
            "Git returned invalid repository metadata.",
        )
    })?;
    let mut lines = probe.lines();
    let inside_work_tree = lines.next();
    let bare = lines.next();
    if inside_work_tree != Some("true") || bare == Some("true") {
        return Err(OrbitError::unsupported(
            "validate_repository",
            "M0 supports non-bare Git working trees only.",
        ));
    }

    let root = runner.run(
        Some(&selected),
        "resolve_repository_root",
        ["--no-optional-locks", "rev-parse", "--show-toplevel"],
        REPOSITORY_PROBE_LIMIT,
    )?;
    let root = runner.require_success("resolve_repository_root", root)?;
    let root = root.stdout.strip_suffix(b"\n").ok_or_else(|| {
        OrbitError::unsupported(
            "resolve_repository_root",
            "Git returned malformed repository root data.",
        )
    })?;
    #[cfg(windows)]
    let root = root.strip_suffix(b"\r").unwrap_or(root);
    let root = std::str::from_utf8(root).map_err(|_| {
        OrbitError::unsupported(
            "resolve_repository_root",
            "The repository root path is not valid UTF-8.",
        )
    })?;
    let root = PathBuf::from(root);

    fs::canonicalize(root).map_err(|_| {
        OrbitError::repository_unavailable("The Git working-tree root cannot be read.")
    })
}

fn has_repository_marker(selected: &Path) -> bool {
    selected.ancestors().any(|directory| {
        let marker = directory.join(".git");
        fs::metadata(&marker).is_ok_and(|metadata| {
            metadata.is_file() || (metadata.is_dir() && marker.join("HEAD").exists())
        })
    })
}

fn build_snapshot(
    runner: &GitRunner,
    repository_id: String,
    root: &Path,
) -> Result<RepositorySnapshot, OrbitError> {
    let root_string = root.to_str().ok_or_else(|| {
        OrbitError::unsupported(
            "read_repository",
            "The repository root path is not valid UTF-8.",
        )
    })?;
    let status = read_status(runner, root)?;
    let recent_commits = read_recent_commits(runner, root, status.head.oid.is_some())?;
    let display_name = root
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(root_string)
        .to_owned();

    Ok(RepositorySnapshot {
        repository_id,
        root: root_string.to_owned(),
        display_name,
        head: status.head,
        working_tree: status.working_tree,
        recent_commits,
    })
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashSet,
        ffi::{OsStr, OsString},
        sync::{
            atomic::{AtomicU64, Ordering},
            Arc,
        },
        thread,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    #[cfg(unix)]
    use std::os::unix::{ffi::OsStringExt, fs::PermissionsExt};

    use crate::git::{
        history_starting_tips, read_graph_commit_page, read_graph_order, read_object_format,
        ChangeKind, CommitHistoryHead, CommitRefKind, ConflictKind, DiffLineKind,
        DiffUnavailableReason, FileDiffContent,
    };
    use crate::history_sessions::MAX_HISTORY_SESSION_COMMITS;

    use super::*;

    static NEXT_TEST_REPOSITORY: AtomicU64 = AtomicU64::new(1);

    struct TestRepository {
        root: PathBuf,
    }

    impl TestRepository {
        fn new() -> Self {
            let nonce = NEXT_TEST_REPOSITORY.fetch_add(1, Ordering::Relaxed);
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock")
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "orbit-m0-test-{}-{timestamp}-{nonce}",
                std::process::id()
            ));
            fs::create_dir(&root).expect("create test directory");
            let repository = Self { root };
            repository.git_ok(["init", "-b", "main"]);
            repository.git_ok(["config", "user.name", "Orbit Tests"]);
            repository.git_ok(["config", "user.email", "orbit@example.test"]);
            repository
        }

        fn write(&self, path: &str, contents: &str) {
            fs::write(self.root.join(path), contents).expect("write fixture");
        }

        fn write_path(&self, path: &Path, contents: &[u8]) {
            fs::write(self.root.join(path), contents).expect("write byte-path fixture");
        }

        fn commit_file(&self, path: &str, contents: &str, message: &str) {
            self.write(path, contents);
            self.git_ok(["add", "--", path]);
            self.git_ok(["commit", "-m", message]);
        }

        fn commit_object(&self, parents: &[&str], timestamp: i64, subject: &str) -> String {
            let tree = self.git_output(["mktree"]);
            let parent_headers = parents
                .iter()
                .map(|parent| format!("parent {parent}\n"))
                .collect::<String>();
            let raw = format!(
                "tree {tree}\n{parent_headers}\
                 author Orbit Tests <orbit@example.test> {timestamp} +0000\n\
                 committer Orbit Tests <orbit@example.test> {timestamp} +0000\n\
                 \n{subject}\n"
            );
            let fixture = format!("raw-commit-{timestamp}");
            self.write(&fixture, &raw);
            let oid =
                self.git_output(["hash-object", "-t", "commit", "-w", "--", fixture.as_str()]);
            fs::remove_file(self.root.join(fixture)).expect("remove raw commit fixture");
            oid
        }

        fn git_ok<const N: usize>(&self, args: [&str; N]) {
            self.git_output(args);
        }

        fn git_output<const N: usize>(&self, args: [&str; N]) -> String {
            let output = GitRunner::default()
                .run(Some(&self.root), "test_fixture", args, 1024 * 1024)
                .expect("run fixture git command");
            assert!(
                output.status.success(),
                "fixture git command failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout)
                .expect("fixture output should be UTF-8")
                .trim_end_matches(['\r', '\n'])
                .to_owned()
        }

        fn git_bytes<I, S>(&self, args: I) -> Vec<u8>
        where
            I: IntoIterator<Item = S>,
            S: AsRef<OsStr>,
        {
            let output = GitRunner::default()
                .run(Some(&self.root), "test_fixture", args, 1024 * 1024)
                .expect("run byte-safe fixture git command");
            assert!(output.status.success(), "fixture Git command failed");
            output.stdout
        }

        fn oid(&self, revision: &str) -> String {
            self.git_output(["rev-parse", "--verify", revision])
        }

        fn git_path(&self, name: &str) -> PathBuf {
            let path = PathBuf::from(self.git_output([
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                name,
            ]));
            if path.is_absolute() {
                path
            } else {
                self.root.join(path)
            }
        }

        fn git_failure<const N: usize>(&self, args: [&str; N]) {
            let output = GitRunner::default()
                .run(Some(&self.root), "test_fixture", args, 1024 * 1024)
                .expect("run failing fixture git command");
            assert!(!output.status.success());
        }
    }

    impl Drop for TestRepository {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).expect("remove test repository");
        }
    }

    fn changed_file<'a>(
        changes: &'a RepositoryChanges,
        display_path: &str,
    ) -> &'a crate::change_sets::ChangedFile {
        changes
            .files
            .iter()
            .find(|file| file.path.text == display_path)
            .unwrap_or_else(|| panic!("missing change entry for {display_path:?}"))
    }

    fn wait_for_path(path: &Path) {
        for _ in 0..300 {
            if path.exists() {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out waiting for {}", path.display());
    }

    #[test]
    fn reads_clean_modified_staged_unstaged_and_unusual_files() {
        let repository = TestRepository::new();
        repository.commit_file("tracked.txt", "base\n", "Initial commit");
        let registry = RepositoryRegistry::default();
        let clean = registry
            .open(repository.root.clone())
            .expect("open clean repo");

        assert!(clean.working_tree.clean);
        assert_eq!(clean.recent_commits.len(), 1);
        assert_eq!(clean.recent_commits[0].subject, "Initial commit");

        repository.write("tracked.txt", "staged\n");
        let modified = registry
            .snapshot(&clean.repository_id)
            .expect("refresh modified repo");
        assert_eq!(modified.working_tree.staged, 0);
        assert_eq!(modified.working_tree.unstaged, 1);

        repository.git_ok(["add", "--", "tracked.txt"]);
        repository.write("tracked.txt", "staged and unstaged\n");
        for name in [
            "space name.txt",
            "unicodé.txt",
            "-leading.txt",
            "line\nbreak.txt",
        ] {
            repository.write(name, "untracked\n");
        }

        let dirty = registry
            .snapshot(&clean.repository_id)
            .expect("refresh dirty repo");
        assert_eq!(dirty.working_tree.staged, 1);
        assert_eq!(dirty.working_tree.unstaged, 1);
        assert_eq!(dirty.working_tree.untracked, 4);
        assert_eq!(dirty.working_tree.conflicted, 0);
        assert!(!dirty.working_tree.clean);
    }

    #[test]
    fn reads_an_unborn_repository_and_detached_head() {
        let unborn_repository = TestRepository::new();
        let registry = RepositoryRegistry::default();
        let unborn = registry
            .open(unborn_repository.root.clone())
            .expect("open unborn repo");

        assert_eq!(unborn.head.oid, None);
        assert_eq!(unborn.head.branch.as_deref(), Some("main"));
        assert!(unborn.recent_commits.is_empty());

        let detached_repository = TestRepository::new();
        detached_repository.commit_file("tracked.txt", "base\n", "Initial commit");
        detached_repository.git_ok(["checkout", "--detach", "HEAD"]);
        let detached = registry
            .open(detached_repository.root.clone())
            .expect("open detached repo");

        assert!(detached.head.detached);
        assert_eq!(detached.head.branch, None);
        assert!(detached.head.oid.is_some());
    }

    #[test]
    fn reads_a_real_conflict() {
        let repository = TestRepository::new();
        repository.commit_file("conflict.txt", "base\n", "Base");
        repository.git_ok(["checkout", "-b", "feature"]);
        repository.commit_file("conflict.txt", "feature\n", "Feature");
        repository.git_ok(["checkout", "main"]);
        repository.commit_file("conflict.txt", "main\n", "Main");
        repository.git_failure(["merge", "feature"]);

        let snapshot = RepositoryRegistry::default()
            .open(repository.root.clone())
            .expect("open conflicted repo");

        assert_eq!(snapshot.working_tree.conflicted, 1);
        assert!(!snapshot.working_tree.clean);
    }

    #[test]
    fn status_does_not_execute_configured_content_filters() {
        let repository = TestRepository::new();
        repository.write(".gitattributes", "tracked.txt filter=orbit-test\n");
        repository.write("tracked.txt", "base\n");
        repository.git_ok(["add", "--", ".gitattributes", "tracked.txt"]);
        repository.git_ok(["commit", "-m", "Initial commit"]);

        let marker = repository.root.join("filter-ran");
        let command = format!("touch {}", marker.display());
        repository.git_ok(["config", "filter.orbit-test.clean", command.as_str()]);
        repository.git_ok(["config", "filter.orbit-test.process", command.as_str()]);
        repository.git_ok(["config", "filter.orbit-test.required", "true"]);
        repository.write("tracked.txt", "next\n");

        GitRunner::default()
            .run(
                Some(&repository.root),
                "test_unprotected_status",
                [
                    "-c",
                    "core.fsmonitor=false",
                    "--no-optional-locks",
                    "status",
                    "--porcelain=v2",
                    "-z",
                ],
                1024 * 1024,
            )
            .expect("run unprotected status fixture");
        assert!(marker.exists(), "fixture filter was not executed");
        fs::remove_file(&marker).expect("remove filter marker");

        let snapshot = RepositoryRegistry::default()
            .open(repository.root.clone())
            .expect("open repository without executing filters");

        assert_eq!(snapshot.working_tree.unstaged, 1);
        assert!(!marker.exists());

        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(repository.root.clone())
            .expect("authorize filtered repository");
        registry
            .repository_changes(&opened.repository_id)
            .expect("read detailed status without executing filters");
        assert!(!marker.exists());
    }

    #[test]
    fn inherited_git_environment_cannot_redirect_repository_identity() {
        let selected = TestRepository::new();
        selected.commit_file("selected.txt", "selected\n", "Selected repository");
        let redirected = TestRepository::new();
        redirected.commit_file("redirected.txt", "redirected\n", "Redirected repository");
        let runner = GitRunner::default()
            .with_environment("GIT_DIR", redirected.root.join(".git").into_os_string())
            .with_environment("GIT_WORK_TREE", redirected.root.clone().into_os_string());

        let registry = RepositoryRegistry::with_runner(runner);
        let snapshot = registry
            .open(selected.root.clone())
            .expect("open selected repository");

        assert_eq!(snapshot.root, selected.root.to_string_lossy());
        assert_eq!(snapshot.recent_commits[0].subject, "Selected repository");
        let changes = registry
            .repository_changes(&snapshot.repository_id)
            .expect("read selected repository changes");
        assert_eq!(changes.repository_id, snapshot.repository_id);
        let history = registry
            .commit_history_page(&snapshot.repository_id, None, Some(1))
            .expect("read selected repository history");
        assert_eq!(history.commits[0].subject, "Selected repository");
    }

    #[cfg(unix)]
    #[test]
    fn history_does_not_execute_configured_signature_verification() {
        use std::os::unix::fs::PermissionsExt;

        let repository = TestRepository::new();
        repository.commit_file("tracked.txt", "base\n", "Base");
        let parent = repository.oid("HEAD");
        let tree = repository.git_output(["rev-parse", "HEAD^{tree}"]);
        let raw_commit = format!(
            "tree {tree}\n\
             parent {parent}\n\
             author Orbit Tests <orbit@example.test> 1700000000 +0000\n\
             committer Orbit Tests <orbit@example.test> 1700000000 +0000\n\
             gpgsig -----BEGIN PGP SIGNATURE-----\n\
              invalid test signature\n\
              -----END PGP SIGNATURE-----\n\
             \n\
             Signed fixture\n"
        );
        repository.write("signed-commit-object", &raw_commit);
        let signed_oid = repository.git_output([
            "hash-object",
            "-t",
            "commit",
            "-w",
            "--",
            "signed-commit-object",
        ]);
        repository.git_ok(["update-ref", "refs/heads/main", signed_oid.as_str()]);

        let marker = repository.root.join("signature-verifier-ran");
        let verifier = repository.root.join("fake-gpg");
        repository.write(
            "fake-gpg",
            &format!("#!/bin/sh\n: > '{}'\nexit 1\n", marker.display()),
        );
        let mut permissions = fs::metadata(&verifier)
            .expect("signature verifier metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&verifier, permissions).expect("make verifier executable");
        let verifier = verifier.to_str().expect("UTF-8 verifier path");
        repository.git_ok(["config", "gpg.program", verifier]);
        repository.git_ok(["config", "log.showSignature", "true"]);

        let unprotected = GitRunner::default()
            .run(
                Some(&repository.root),
                "test_unprotected_history",
                ["--no-pager", "log", "-1", "--format=%H", "HEAD", "--"],
                1024 * 1024,
            )
            .expect("run unprotected history fixture");
        assert!(unprotected.status.success());
        assert!(marker.exists(), "fixture verifier was not executed");
        fs::remove_file(&marker).expect("remove signature marker");

        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(repository.root.clone())
            .expect("open signed history repository");
        registry
            .commit_history_page(&opened.repository_id, None, Some(1))
            .expect("read protected commit history");

        assert!(!marker.exists());
    }

    #[cfg(unix)]
    #[test]
    fn graph_history_does_not_execute_textconv_or_external_diff_programs() {
        use std::os::unix::fs::PermissionsExt;

        let repository = TestRepository::new();
        repository.write(".gitattributes", "tracked.txt diff=orbit-test\n");
        repository.write("tracked.txt", "base\n");
        repository.git_ok(["add", "--", ".gitattributes", "tracked.txt"]);
        repository.git_ok(["commit", "-m", "Base"]);
        repository.commit_file("tracked.txt", "next\n", "Change tracked file");

        let textconv_marker = repository.root.join("textconv-ran");
        let textconv = repository.root.join("fake-textconv");
        repository.write(
            "fake-textconv",
            &format!(
                "#!/bin/sh\n: > '{}'\ncat \"$1\"\n",
                textconv_marker.display()
            ),
        );
        let mut permissions = fs::metadata(&textconv)
            .expect("textconv metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&textconv, permissions).expect("make textconv executable");
        repository.git_ok([
            "config",
            "diff.orbit-test.textconv",
            textconv.to_str().expect("UTF-8 textconv path"),
        ]);

        let unsafe_textconv = GitRunner::default()
            .run(
                Some(&repository.root),
                "test_unprotected_textconv",
                ["--no-pager", "log", "-p", "--textconv", "-1", "HEAD", "--"],
                1024 * 1024,
            )
            .expect("run unprotected textconv fixture");
        assert!(unsafe_textconv.status.success());
        assert!(
            textconv_marker.exists(),
            "fixture textconv was not executed"
        );
        fs::remove_file(&textconv_marker).expect("remove textconv marker");

        let external_marker = repository.root.join("external-diff-ran");
        let external_diff = repository.root.join("fake-external-diff");
        repository.write(
            "fake-external-diff",
            &format!("#!/bin/sh\n: > '{}'\nexit 0\n", external_marker.display()),
        );
        let mut permissions = fs::metadata(&external_diff)
            .expect("external diff metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&external_diff, permissions).expect("make external diff executable");
        repository.git_ok([
            "config",
            "diff.external",
            external_diff.to_str().expect("UTF-8 external diff path"),
        ]);

        let unsafe_external_diff = GitRunner::default()
            .run(
                Some(&repository.root),
                "test_unprotected_external_diff",
                ["--no-pager", "log", "-p", "--ext-diff", "-1", "HEAD", "--"],
                1024 * 1024,
            )
            .expect("run unprotected external diff fixture");
        assert!(unsafe_external_diff.status.success());
        assert!(
            external_marker.exists(),
            "fixture external diff was not executed"
        );
        fs::remove_file(&external_marker).expect("remove external diff marker");

        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(repository.root.clone())
            .expect("open configured repository");
        registry
            .commit_history_page(&opened.repository_id, None, Some(2))
            .expect("read protected graph history");

        assert!(!textconv_marker.exists());
        assert!(!external_marker.exists());
    }

    #[cfg(unix)]
    #[test]
    fn repository_reads_do_not_execute_configured_fsmonitor_programs() {
        use std::os::unix::fs::PermissionsExt;

        let repository = TestRepository::new();
        repository.commit_file("tracked.txt", "base\n", "Base");

        let fsmonitor_marker = repository.root.join("fsmonitor-ran");
        let fsmonitor = repository.root.join("fake-fsmonitor");
        repository.write(
            "fake-fsmonitor",
            &format!("#!/bin/sh\n: > '{}'\nexit 0\n", fsmonitor_marker.display()),
        );
        let mut permissions = fs::metadata(&fsmonitor)
            .expect("fsmonitor metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&fsmonitor, permissions).expect("make fsmonitor executable");
        repository.git_ok([
            "config",
            "core.fsmonitor",
            fsmonitor.to_str().expect("UTF-8 fsmonitor path"),
        ]);

        let unsafe_status = GitRunner::default()
            .run(
                Some(&repository.root),
                "test_unprotected_fsmonitor",
                ["--no-optional-locks", "status", "--porcelain=v2", "-z"],
                1024 * 1024,
            )
            .expect("run unprotected fsmonitor fixture");
        assert!(unsafe_status.status.success());
        assert!(
            fsmonitor_marker.exists(),
            "fixture fsmonitor was not executed"
        );
        fs::remove_file(&fsmonitor_marker).expect("remove fsmonitor marker");

        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(repository.root.clone())
            .expect("open configured repository");
        registry
            .repository_changes(&opened.repository_id)
            .expect("read protected detailed status");
        registry
            .commit_history_page(&opened.repository_id, None, Some(1))
            .expect("read protected graph history");

        assert!(!fsmonitor_marker.exists());
    }

    #[cfg(unix)]
    #[test]
    fn graph_history_does_not_lazy_fetch_missing_promisor_objects() {
        use std::os::unix::fs::PermissionsExt;

        let repository = TestRepository::new();
        repository.commit_file("tracked.txt", "base\n", "Base");
        let missing_oid = repository.oid("HEAD");
        let object_path = repository
            .root
            .join(".git/objects")
            .join(&missing_oid[..2])
            .join(&missing_oid[2..]);
        assert!(object_path.is_file(), "fixture commit should be loose");

        let marker = repository.root.join("lazy-fetch-ran");
        let remote = repository.root.join("fake-promisor-remote");
        repository.write(
            "fake-promisor-remote",
            &format!("#!/bin/sh\n: > '{}'\nexit 1\n", marker.display()),
        );
        let mut permissions = fs::metadata(&remote)
            .expect("promisor remote metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&remote, permissions).expect("make promisor remote executable");
        repository.git_ok(["config", "core.repositoryformatversion", "1"]);
        repository.git_ok(["config", "extensions.partialClone", "origin"]);
        repository.git_ok(["config", "remote.origin.promisor", "true"]);
        repository.git_ok(["config", "remote.origin.partialclonefilter", "blob:none"]);
        repository.git_ok([
            "config",
            "remote.origin.url",
            &format!("ext::{}", remote.display()),
        ]);
        repository.git_ok(["config", "protocol.ext.allow", "always"]);
        fs::remove_file(&object_path).expect("remove promised commit object");

        let unsafe_history = GitRunner::default()
            .without_no_lazy_fetch_environment()
            .run(
                Some(&repository.root),
                "test_unprotected_lazy_fetch",
                [
                    "--no-pager",
                    "log",
                    "-1",
                    "--format=%H",
                    missing_oid.as_str(),
                    "--",
                ],
                1024 * 1024,
            )
            .expect("run unprotected lazy-fetch fixture");
        assert!(!unsafe_history.status.success());
        assert!(marker.exists(), "fixture promisor remote was not executed");
        fs::remove_file(&marker).expect("remove lazy-fetch marker");

        let error = read_graph_commit_page(
            &GitRunner::default(),
            &repository.root,
            &[missing_oid],
            crate::git::ObjectFormat::Sha1,
        )
        .expect_err("missing promised object should fail without fetching");

        assert_eq!(error.code, "git_command_failed");
        assert!(!marker.exists());
    }

    #[test]
    fn preserves_a_trailing_newline_in_the_repository_root() {
        let mut repository = TestRepository::new();
        let renamed = repository.root.with_file_name(format!(
            "{}\n",
            repository
                .root
                .file_name()
                .expect("test repository name")
                .to_string_lossy()
        ));
        fs::rename(&repository.root, &renamed).expect("rename test repository");
        repository.root = renamed;
        repository.commit_file("tracked.txt", "base\n", "Initial commit");

        let snapshot = RepositoryRegistry::default()
            .open(repository.root.clone())
            .expect("open repository with newline suffix");

        assert_eq!(snapshot.root, repository.root.to_string_lossy());
    }

    #[test]
    fn returns_structured_errors_for_invalid_and_bare_directories() {
        let directory = TestRepository::new();
        fs::remove_dir_all(directory.root.join(".git")).expect("remove fixture metadata");
        let invalid = RepositoryRegistry::default()
            .open(directory.root.clone())
            .expect_err("invalid directory");
        assert_eq!(invalid.code, "not_a_repository");

        let bare = TestRepository::new();
        fs::remove_dir_all(bare.root.join(".git")).expect("remove fixture metadata");
        bare.git_ok(["init", "--bare"]);
        let unsupported = RepositoryRegistry::default()
            .open(bare.root.clone())
            .expect_err("bare repository");
        assert_eq!(unsupported.code, "unsupported_repository_state");
    }

    #[test]
    fn preserves_git_failures_for_discovered_repository_metadata() {
        let repository = TestRepository::new();
        fs::write(repository.root.join(".git/config"), "[invalid\n")
            .expect("corrupt fixture config");

        let error = RepositoryRegistry::default()
            .open(repository.root.clone())
            .expect_err("invalid repository configuration");

        assert_eq!(error.code, "git_command_failed");
        assert_eq!(error.operation, "validate_repository");
        assert!(error.details.is_some());
    }

    #[test]
    fn rejects_unknown_repository_ids() {
        let error = RepositoryRegistry::default()
            .snapshot("repository-not-authorized")
            .expect_err("unknown repository ID");

        assert_eq!(error.code, "repository_unavailable");
    }

    #[test]
    fn maps_a_missing_git_executable_during_open() {
        let repository = TestRepository::new();
        let registry =
            RepositoryRegistry::with_runner(GitRunner::with_executable("orbit-git-does-not-exist"));
        let error = registry
            .open(repository.root.clone())
            .expect_err("missing executable");

        assert_eq!(error.code, "git_not_found");
    }

    #[test]
    fn paginated_history_preserves_one_shot_topo_order_across_sensitive_boundaries() {
        let repository = TestRepository::new();
        let root = repository.commit_object(&[], 1_700_002_364, "root");
        let main_line = repository.commit_object(&[root.as_str()], 1_700_006_392, "main-line");
        let merge =
            repository.commit_object(&[main_line.as_str(), root.as_str()], 1_700_000_705, "merge");
        let independent = repository.commit_object(&[], 1_700_003_885, "independent");
        let side = repository.commit_object(
            &[merge.as_str(), independent.as_str()],
            1_700_009_333,
            "side",
        );
        let head = repository.commit_object(&[root.as_str()], 1_700_004_553, "head");
        repository.git_ok(["update-ref", "refs/heads/main", head.as_str()]);
        repository.git_ok(["update-ref", "refs/heads/side", side.as_str()]);

        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(repository.root.clone())
            .expect("open ordering fixture");
        let object_format = read_object_format(&registry.runner, &repository.root)
            .expect("read fixture object format");
        let expected = read_graph_order(
            &registry.runner,
            &repository.root,
            &[head.clone(), side.clone()],
            MAX_HISTORY_SESSION_COMMITS + 1,
            object_format,
        )
        .expect("one-shot sensitive topological order");
        assert_eq!(
            expected,
            vec![side, independent, merge, main_line, head, root],
            "fixture must exercise Git's page-boundary-sensitive ordering"
        );

        for page_size in 1..=5 {
            let first = registry
                .commit_history_page(&opened.repository_id, None, Some(page_size))
                .expect("first sensitive-order page");
            let mut actual = first
                .commits
                .into_iter()
                .map(|commit| commit.oid)
                .collect::<Vec<_>>();
            let mut cursor = first.next_cursor;
            while let Some(current) = cursor {
                let page = registry
                    .commit_history_page(
                        &opened.repository_id,
                        Some(current.as_str()),
                        Some(page_size),
                    )
                    .expect("continue sensitive-order history");
                actual.extend(page.commits.into_iter().map(|commit| commit.oid));
                cursor = page.next_cursor;
            }

            assert_eq!(actual, expected, "page size {page_size}");
        }
    }

    #[test]
    fn reads_paginated_nonlinear_history_and_typed_refs() {
        let repository = TestRepository::new();
        repository.commit_file("base.txt", "base\n", "Base");
        let base_oid = repository.oid("HEAD");

        repository.git_ok(["checkout", "-b", "feature-one"]);
        repository.commit_file("feature-one.txt", "feature one\n", "Feature one");
        repository.git_ok(["checkout", "main"]);
        repository.commit_file("main-one.txt", "main one\n", "Main one");
        repository.git_ok(["merge", "--no-ff", "feature-one", "-m", "Merge one"]);

        repository.git_ok(["checkout", "-b", "feature-two"]);
        repository.commit_file("feature-two.txt", "feature two\n", "Feature two");
        repository.git_ok(["checkout", "main"]);
        repository.git_ok(["checkout", "-b", "feature-three"]);
        repository.commit_file("feature-three.txt", "feature three\n", "Feature three");
        repository.git_ok(["checkout", "main"]);
        repository.git_ok([
            "merge",
            "--no-ff",
            "feature-two",
            "feature-three",
            "-m",
            "Octopus merge",
        ]);

        repository.git_ok(["checkout", "-b", "nested"]);
        repository.commit_file("nested.txt", "nested\n", "Nested branch");
        repository.git_ok(["checkout", "main"]);
        repository.commit_file("main-two.txt", "main two\n", "Main two");
        repository.git_ok(["merge", "--no-ff", "nested", "-m", "Merge two"]);
        let tip_oid = repository.oid("HEAD");

        repository.git_ok(["branch", "naïve/topic", base_oid.as_str()]);
        repository.git_ok(["tag", "lightweight", tip_oid.as_str()]);
        repository.git_ok([
            "tag",
            "-a",
            "annotated/β",
            "-m",
            "Annotated tag",
            base_oid.as_str(),
        ]);
        repository.git_ok(["update-ref", "refs/remotes/origin/main", tip_oid.as_str()]);
        repository.git_ok([
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ]);
        repository.git_ok(["checkout", "--detach", "HEAD"]);

        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(repository.root.clone())
            .expect("open history repository");
        let first = registry
            .commit_history_page(&opened.repository_id, None, Some(2))
            .expect("first history page");

        assert!(matches!(first.head, CommitHistoryHead::Detached { .. }));
        assert!(first.refs.iter().any(|commit_ref| {
            commit_ref.kind == CommitRefKind::LocalBranch
                && commit_ref.display_name == "naïve/topic"
        }));
        assert!(first.refs.iter().any(|commit_ref| {
            commit_ref.kind == CommitRefKind::RemoteTrackingBranch
                && commit_ref.full_name == "refs/remotes/origin/main"
        }));
        assert!(first.refs.iter().any(|commit_ref| {
            commit_ref.kind == CommitRefKind::SymbolicRef
                && commit_ref.symbolic_target.as_deref() == Some("refs/remotes/origin/main")
        }));
        assert!(first.refs.iter().any(|commit_ref| {
            commit_ref.kind == CommitRefKind::LightweightTag && commit_ref.target_oid == tip_oid
        }));
        assert!(first.refs.iter().any(|commit_ref| {
            commit_ref.kind == CommitRefKind::AnnotatedTag
                && commit_ref.display_name == "annotated/β"
                && commit_ref.target_oid == base_oid
        }));
        assert!(
            first
                .refs
                .iter()
                .filter(|commit_ref| commit_ref.target_oid == tip_oid)
                .count()
                >= 3,
            "multiple refs should be retained on one commit"
        );

        let object_format = read_object_format(&registry.runner, &repository.root)
            .expect("read fixture object format");
        let tips = history_starting_tips(&first.head, &first.refs);
        let expected_order = read_graph_order(
            &registry.runner,
            &repository.root,
            &tips,
            200,
            object_format,
        )
        .expect("one-shot topological order");
        let expected_commits = read_graph_commit_page(
            &registry.runner,
            &repository.root,
            &expected_order,
            object_format,
        )
        .expect("one-shot commit metadata");
        assert!(
            expected_commits
                .iter()
                .any(|commit| commit.parent_oids.len() == 3),
            "fixture should contain an octopus merge"
        );
        let expected = expected_commits
            .into_iter()
            .map(|commit| commit.oid)
            .collect::<Vec<_>>();

        let mut actual = first
            .commits
            .iter()
            .map(|commit| commit.oid.clone())
            .collect::<Vec<_>>();
        let mut cursor = first.next_cursor;
        while let Some(current) = cursor {
            let page = registry
                .commit_history_page(&opened.repository_id, Some(current.as_str()), Some(2))
                .expect("next history page");
            assert!(
                page.refs.is_empty(),
                "refs are returned only on session start"
            );
            actual.extend(page.commits.iter().map(|commit| commit.oid.clone()));
            cursor = page.next_cursor;
        }

        assert_eq!(actual, expected);
        assert_eq!(actual.iter().collect::<HashSet<_>>().len(), actual.len());
    }

    #[test]
    fn represents_unborn_history_without_a_session() {
        let repository = TestRepository::new();
        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(repository.root.clone())
            .expect("open unborn repo");

        let page = registry
            .commit_history_page(&opened.repository_id, None, None)
            .expect("read unborn history");

        assert!(matches!(
            page.head,
            CommitHistoryHead::Unborn { ref branch } if branch == "main"
        ));
        assert!(page.commits.is_empty());
        assert!(page.refs.is_empty());
        assert!(!page.has_more);
        assert!(page.next_cursor.is_none());
    }

    #[test]
    fn validates_history_cursors_page_limits_and_single_use() {
        let repository = TestRepository::new();
        for index in 0..4 {
            repository.commit_file(
                &format!("{index}.txt"),
                &format!("{index}\n"),
                &format!("Commit {index}"),
            );
        }
        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(repository.root.clone())
            .expect("open repository");

        for page_size in [Some(0), Some(201)] {
            let error = registry
                .commit_history_page(&opened.repository_id, None, page_size)
                .expect_err("reject invalid page size");
            assert_eq!(error.code, "invalid_history_request");
        }
        for cursor in ["invalid", "history-000000000000ffff"] {
            let error = registry
                .commit_history_page(&opened.repository_id, Some(cursor), Some(1))
                .expect_err("reject unknown cursor");
            assert_eq!(error.code, "history_session_unavailable");
        }

        let first = registry
            .commit_history_page(&opened.repository_id, None, Some(1))
            .expect("start history");
        let cursor = first.next_cursor.expect("continuation cursor");

        let other_repository = TestRepository::new();
        other_repository.commit_file("other.txt", "other\n", "Other");
        let other = registry
            .open(other_repository.root.clone())
            .expect("open another repository");
        let unauthorized = registry
            .commit_history_page(&other.repository_id, Some(cursor.as_str()), Some(1))
            .expect_err("cursor cannot cross repositories");
        assert_eq!(unauthorized.code, "history_session_unavailable");

        registry
            .commit_history_page(&opened.repository_id, Some(cursor.as_str()), Some(1))
            .expect("consume cursor");
        let reused = registry
            .commit_history_page(&opened.repository_id, Some(cursor.as_str()), Some(1))
            .expect_err("cursor is single-use");
        assert_eq!(reused.code, "history_session_unavailable");

        let fresh = registry
            .commit_history_page(&opened.repository_id, None, Some(1))
            .expect("start expiring history");
        let expiring_cursor = fresh.next_cursor.expect("expiring cursor");
        registry.histories.expire_for_test(expiring_cursor.as_str());
        let expired = registry
            .commit_history_page(
                &opened.repository_id,
                Some(expiring_cursor.as_str()),
                Some(1),
            )
            .expect_err("expired cursor");
        assert_eq!(expired.code, "history_session_unavailable");
    }

    #[test]
    fn failed_page_read_leaves_the_cursor_retryable() {
        let repository = TestRepository::new();
        for index in 0..3 {
            repository.commit_file(
                &format!("retry-{index}.txt"),
                &format!("{index}\n"),
                &format!("Retry {index}"),
            );
        }
        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(repository.root.clone())
            .expect("open retry fixture");
        let first = registry
            .commit_history_page(&opened.repository_id, None, Some(1))
            .expect("start retryable history");
        let cursor = first.next_cursor.expect("continuation cursor");
        let next_oid = repository.oid("HEAD^");
        let object_path = repository
            .root
            .join(".git/objects")
            .join(&next_oid[..2])
            .join(&next_oid[2..]);
        let object = fs::read(&object_path).expect("read loose commit object");
        fs::remove_file(&object_path).expect("temporarily remove commit object");

        let failed = registry
            .commit_history_page(&opened.repository_id, Some(cursor.as_str()), Some(1))
            .expect_err("missing commit should fail the page read");
        assert_eq!(failed.code, "git_command_failed");

        fs::write(&object_path, object).expect("restore commit object");
        let retried = registry
            .commit_history_page(&opened.repository_id, Some(cursor.as_str()), Some(1))
            .expect("same cursor should remain retryable");
        assert_eq!(retried.commits[0].oid, next_oid);
        let reused = registry
            .commit_history_page(&opened.repository_id, Some(cursor.as_str()), Some(1))
            .expect_err("successful retry consumes old cursor");
        assert_eq!(reused.code, "history_session_unavailable");
    }

    #[test]
    fn keeps_a_session_on_its_original_history_when_head_moves() {
        let repository = TestRepository::new();
        for index in 0..4 {
            repository.commit_file(
                &format!("before-{index}.txt"),
                "before\n",
                &format!("Before {index}"),
            );
        }
        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(repository.root.clone())
            .expect("open repository");
        let first = registry
            .commit_history_page(&opened.repository_id, None, Some(1))
            .expect("start history");
        let session_head = first.head.oid().expect("attached HEAD").to_owned();
        let cursor = first.next_cursor.expect("continuation cursor");

        repository.commit_file("after.txt", "after\n", "After session started");
        let moved_head = repository.oid("HEAD");
        assert_ne!(moved_head, session_head);

        let mut paginated = first.commits;
        let mut cursor = Some(cursor);
        while let Some(current) = cursor {
            let page = registry
                .commit_history_page(&opened.repository_id, Some(current.as_str()), Some(1))
                .expect("continue original history");
            assert_eq!(page.head.oid(), Some(session_head.as_str()));
            paginated.extend(page.commits);
            cursor = page.next_cursor;
        }

        assert!(!paginated.iter().any(|commit| commit.oid == moved_head));
        assert_eq!(paginated.len(), 4);
    }

    #[test]
    fn invalidates_history_when_the_authorized_repository_disappears() {
        let mut repository = TestRepository::new();
        repository.commit_file("one.txt", "one\n", "One");
        repository.commit_file("two.txt", "two\n", "Two");
        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(repository.root.clone())
            .expect("open repository");
        let first = registry
            .commit_history_page(&opened.repository_id, None, Some(1))
            .expect("start history");
        let cursor = first.next_cursor.expect("continuation cursor");
        repository.write("untracked.txt", "untracked\n");
        let changes = registry
            .repository_changes(&opened.repository_id)
            .expect("start change set");
        let change_set_id = changes.change_set_id.as_str().to_owned();
        let file_id = changes.files[0].file_id.as_str().to_owned();

        let original = repository.root.clone();
        let moved = original.with_file_name(format!(
            "{}-moved",
            original
                .file_name()
                .expect("repository name")
                .to_string_lossy()
        ));
        fs::rename(&original, &moved).expect("move repository");
        let error = registry
            .commit_history_page(&opened.repository_id, Some(cursor.as_str()), Some(1))
            .expect_err("repository unavailable");
        assert_eq!(error.code, "repository_unavailable");
        fs::rename(&moved, &original).expect("restore repository");
        repository.root = original;

        let invalidated = registry
            .commit_history_page(&opened.repository_id, Some(cursor.as_str()), Some(1))
            .expect_err("unavailable repository invalidates its sessions");
        assert_eq!(invalidated.code, "history_session_unavailable");
        let invalidated_changes = registry
            .changes
            .authorize_for_test(
                &opened.repository_id,
                &repository.root,
                &change_set_id,
                &file_id,
            )
            .expect_err("unavailable repository invalidates its change sets");
        assert_eq!(invalidated_changes.code, "change_set_unavailable");
    }

    #[test]
    fn detailed_changes_are_semantic_bounded_handles_and_replace_atomically() {
        let repository = TestRepository::new();
        repository.commit_file("both.txt", "base\n", "Base");
        repository.commit_file("rename-source.txt", "rename\n", "Rename source");
        repository.commit_file("delete.txt", "delete\n", "Delete source");
        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(repository.root.clone())
            .expect("open detailed-status repository");

        repository.write("both.txt", "staged\n");
        repository.git_ok(["add", "--", "both.txt"]);
        repository.write("both.txt", "staged and unstaged\n");
        repository.git_ok(["mv", "--", "rename-source.txt", "renamed.txt"]);
        repository.git_ok(["rm", "--", "delete.txt"]);
        for name in [
            "space name.txt",
            "unicodé.txt",
            "-leading.txt",
            "line\nbreak.txt",
        ] {
            repository.write(name, "untracked\n");
        }

        let changes = registry
            .repository_changes(&opened.repository_id)
            .expect("read detailed changes");
        let snapshot = registry
            .snapshot(&opened.repository_id)
            .expect("read matching M0 summary");
        assert_eq!(changes.summary, snapshot.working_tree);
        assert_eq!(changes.summary.staged, 3);
        assert_eq!(changes.summary.unstaged, 1);
        assert_eq!(changes.summary.untracked, 4);

        let both = changes
            .files
            .iter()
            .find(|file| file.path.text == "both.txt")
            .expect("both-sided file");
        assert_eq!(
            both.staged.as_ref().map(|facet| facet.kind),
            Some(ChangeKind::Modified)
        );
        assert_eq!(
            both.unstaged.as_ref().map(|facet| facet.kind),
            Some(ChangeKind::Modified)
        );
        let renamed = changes
            .files
            .iter()
            .find(|file| file.path.text == "renamed.txt")
            .expect("renamed file");
        assert_eq!(
            renamed.staged.as_ref().map(|facet| facet.kind),
            Some(ChangeKind::Renamed)
        );
        assert_eq!(
            renamed
                .original_path
                .as_ref()
                .map(|path| path.text.as_str()),
            Some("rename-source.txt")
        );
        let line_break = changes
            .files
            .iter()
            .find(|file| file.path.text == "line\\nbreak.txt")
            .expect("escaped path");
        assert!(line_break.path.escaped);

        let old_change_set = changes.change_set_id.as_str().to_owned();
        let old_file = changes.files[0].file_id.as_str().to_owned();
        let replacement = registry
            .repository_changes(&opened.repository_id)
            .expect("replace detailed changes");
        assert_ne!(replacement.change_set_id.as_str(), old_change_set);
        let old_error = registry
            .changes
            .authorize_for_test(
                &opened.repository_id,
                &repository.root,
                &old_change_set,
                &old_file,
            )
            .expect_err("old change set is stale after successful refresh");
        assert_eq!(old_error.code, "change_set_unavailable");
        registry
            .changes
            .authorize_for_test(
                &opened.repository_id,
                &repository.root,
                replacement.change_set_id.as_str(),
                replacement.files[0].file_id.as_str(),
            )
            .expect("new handles remain authorized");
    }

    #[test]
    fn detailed_changes_return_typed_conflicts() {
        let repository = TestRepository::new();
        repository.commit_file("conflict.txt", "base\n", "Base");
        repository.git_ok(["checkout", "-b", "feature"]);
        repository.commit_file("conflict.txt", "feature\n", "Feature");
        repository.git_ok(["checkout", "main"]);
        repository.commit_file("conflict.txt", "main\n", "Main");
        repository.git_failure(["merge", "feature"]);
        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(repository.root.clone())
            .expect("open conflicted repository");

        let changes = registry
            .repository_changes(&opened.repository_id)
            .expect("read conflicted changes");
        assert_eq!(changes.files.len(), 1);
        assert_eq!(changes.files[0].conflict, Some(ConflictKind::BothModified));
        assert_eq!(changes.files[0].staged, None);
        assert_eq!(changes.files[0].unstaged, None);
    }

    #[cfg(unix)]
    #[test]
    fn detailed_changes_escape_invalid_path_bytes_and_report_type_changes() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt, os::unix::fs::symlink};

        let repository = TestRepository::new();
        repository.commit_file("mode.txt", "mode\n", "Mode");
        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(repository.root.clone())
            .expect("open byte-path repository");
        let invalid_name = OsString::from_vec(b"invalid-\xff.txt".to_vec());
        fs::write(repository.root.join(invalid_name), b"invalid\n")
            .expect("write invalid-byte path");
        fs::remove_file(repository.root.join("mode.txt")).expect("remove regular file");
        symlink("target", repository.root.join("mode.txt")).expect("replace with symlink");

        let changes = registry
            .repository_changes(&opened.repository_id)
            .expect("read byte-safe changes");
        let invalid = changes
            .files
            .iter()
            .find(|file| file.path.text == "invalid-\\xFF.txt")
            .expect("escaped invalid-byte file");
        assert!(invalid.path.escaped);
        let mode = changes
            .files
            .iter()
            .find(|file| file.path.text == "mode.txt")
            .expect("type-changed file");
        assert_eq!(
            mode.unstaged.as_ref().map(|facet| facet.kind),
            Some(ChangeKind::TypeChanged)
        );
        assert_eq!(
            mode.unstaged
                .as_ref()
                .and_then(|facet| facet.old_mode.as_deref()),
            Some("100644")
        );
        assert_eq!(
            mode.unstaged
                .as_ref()
                .and_then(|facet| facet.new_mode.as_deref()),
            Some("120000")
        );
    }

    #[test]
    fn detailed_changes_require_an_authorized_repository() {
        let error = RepositoryRegistry::default()
            .repository_changes("repository-000000000000ffff")
            .expect_err("unknown repository is unauthorized");
        assert_eq!(error.code, "repository_unavailable");
    }

    #[test]
    fn detailed_changes_detect_real_staged_copies() {
        let repository = TestRepository::new();
        repository.commit_file("source.txt", "base\n", "Copy source");
        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(repository.root.clone())
            .expect("open copy repository");
        repository.write("source.txt", "base\nnew\n");
        repository.write("copy.txt", "base\nnew\n");
        repository.git_ok(["add", "--", "source.txt", "copy.txt"]);

        let changes = registry
            .repository_changes(&opened.repository_id)
            .expect("read copy changes");
        let copied = changes
            .files
            .iter()
            .find(|file| file.path.text == "copy.txt")
            .expect("copy entry");
        assert_eq!(
            copied.staged.as_ref().map(|facet| facet.kind),
            Some(ChangeKind::Copied)
        );
        assert_eq!(
            copied.original_path.as_ref().map(|path| path.text.as_str()),
            Some("source.txt")
        );
    }

    #[test]
    fn detailed_changes_handle_an_unborn_repository() {
        let repository = TestRepository::new();
        repository.write("first.txt", "first\n");
        repository.git_ok(["add", "--", "first.txt"]);
        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(repository.root.clone())
            .expect("open unborn repository");

        let changes = registry
            .repository_changes(&opened.repository_id)
            .expect("read unborn changes");
        assert_eq!(changes.head.oid, None);
        assert_eq!(changes.head.branch.as_deref(), Some("main"));
        assert_eq!(changes.files.len(), 1);
        assert_eq!(
            changes.files[0].staged.as_ref().map(|facet| facet.kind),
            Some(ChangeKind::Added)
        );
    }

    #[test]
    fn failed_detailed_refresh_preserves_the_prior_change_set() {
        let repository = TestRepository::new();
        repository.commit_file("tracked.txt", "base\n", "Base");
        repository.write("tracked.txt", "changed\n");
        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(repository.root.clone())
            .expect("open refresh repository");
        let first = registry
            .repository_changes(&opened.repository_id)
            .expect("create first change set");
        let change_set_id = first.change_set_id.as_str().to_owned();
        let file_id = first.files[0].file_id.as_str().to_owned();

        let oversized_driver = "a".repeat(1025);
        let key = format!("filter.{oversized_driver}.clean");
        repository.git_ok(["config", key.as_str(), "cat"]);
        let failed = registry
            .repository_changes(&opened.repository_id)
            .expect_err("oversized filter name rejects refreshed status");
        assert_eq!(failed.code, "git_command_failed");
        registry
            .changes
            .authorize_for_test(
                &opened.repository_id,
                &repository.root,
                &change_set_id,
                &file_id,
            )
            .expect("failed refresh leaves old handles authorized");
    }

    #[test]
    fn selected_diff_reads_staged_and_unstaged_sides_independently() {
        let repository = TestRepository::new();
        repository.commit_file("both.txt", "base\n", "Base");
        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(repository.root.clone())
            .expect("open repository");

        repository.write("both.txt", "staged\n");
        repository.git_ok(["add", "--", "both.txt"]);
        repository.write("both.txt", "worktree\n");
        let changes = registry
            .repository_changes(&opened.repository_id)
            .expect("read changes");
        let file = changes
            .files
            .iter()
            .find(|file| file.path.text == "both.txt")
            .expect("both-sided entry");

        let staged = registry
            .file_diff(
                &opened.repository_id,
                changes.change_set_id.as_str(),
                file.file_id.as_str(),
                DiffSide::Staged,
            )
            .expect("staged diff");
        let unstaged = registry
            .file_diff(
                &opened.repository_id,
                changes.change_set_id.as_str(),
                file.file_id.as_str(),
                DiffSide::Unstaged,
            )
            .expect("unstaged diff");

        let FileDiffContent::Text { hunks, .. } = staged.content else {
            panic!("staged side should be text");
        };
        assert!(hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .any(|line| { line.kind == DiffLineKind::Addition && line.content == "staged" }));
        let FileDiffContent::Text { hunks, .. } = unstaged.content else {
            panic!("unstaged side should be text");
        };
        assert!(hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .any(|line| { line.kind == DiffLineKind::Addition && line.content == "worktree" }));
    }

    #[test]
    fn selected_diff_reads_staged_add_delete_and_unborn_add() {
        let repository = TestRepository::new();
        repository.commit_file("deleted.txt", "delete me\n", "Base");
        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(repository.root.clone())
            .expect("open repository");
        fs::remove_file(repository.root.join("deleted.txt")).expect("delete tracked file");
        repository.write("added.txt", "new\n");
        repository.git_ok(["add", "--all"]);
        let changes = registry
            .repository_changes(&opened.repository_id)
            .expect("read staged changes");

        for path in ["added.txt", "deleted.txt"] {
            let file = changes
                .files
                .iter()
                .find(|file| file.path.text == path)
                .expect("staged entry");
            let diff = registry
                .file_diff(
                    &opened.repository_id,
                    changes.change_set_id.as_str(),
                    file.file_id.as_str(),
                    DiffSide::Staged,
                )
                .expect("staged diff");
            assert!(matches!(diff.content, FileDiffContent::Text { .. }));
        }

        let unborn_repository = TestRepository::new();
        unborn_repository.write("first.txt", "first\n");
        unborn_repository.git_ok(["add", "--", "first.txt"]);
        let unborn_registry = RepositoryRegistry::default();
        let unborn = unborn_registry
            .open(unborn_repository.root.clone())
            .expect("open unborn repository");
        let changes = unborn_registry
            .repository_changes(&unborn.repository_id)
            .expect("read unborn changes");
        let diff = unborn_registry
            .file_diff(
                &unborn.repository_id,
                changes.change_set_id.as_str(),
                changes.files[0].file_id.as_str(),
                DiffSide::Staged,
            )
            .expect("unborn staged diff");
        assert!(matches!(diff.content, FileDiffContent::Text { .. }));
    }

    #[test]
    fn selected_diff_reads_unstaged_delete_and_staged_rename_and_copy() {
        let deleted_repository = TestRepository::new();
        deleted_repository.commit_file("gone.txt", "gone\n", "Base");
        let deleted_registry = RepositoryRegistry::default();
        let deleted = deleted_registry
            .open(deleted_repository.root.clone())
            .expect("open delete repository");
        fs::remove_file(deleted_repository.root.join("gone.txt")).expect("delete tracked file");
        let changes = deleted_registry
            .repository_changes(&deleted.repository_id)
            .expect("read deletion");
        let diff = deleted_registry
            .file_diff(
                &deleted.repository_id,
                changes.change_set_id.as_str(),
                changes.files[0].file_id.as_str(),
                DiffSide::Unstaged,
            )
            .expect("unstaged deletion diff");
        assert!(matches!(diff.content, FileDiffContent::Text { .. }));

        let movement_repository = TestRepository::new();
        movement_repository.commit_file("old.txt", "same\n", "Base");
        let movement_registry = RepositoryRegistry::default();
        let movement = movement_registry
            .open(movement_repository.root.clone())
            .expect("open movement repository");
        movement_repository.git_ok(["mv", "old.txt", "renamed.txt"]);
        let changes = movement_registry
            .repository_changes(&movement.repository_id)
            .expect("read rename");
        let renamed = changes
            .files
            .iter()
            .find(|file| file.path.text == "renamed.txt")
            .expect("rename entry");
        let diff = movement_registry
            .file_diff(
                &movement.repository_id,
                changes.change_set_id.as_str(),
                renamed.file_id.as_str(),
                DiffSide::Staged,
            )
            .expect("rename diff");
        let FileDiffContent::Text { metadata, .. } = diff.content else {
            panic!("rename should be text metadata");
        };
        assert!(metadata.renamed);

        let copy_repository = TestRepository::new();
        copy_repository.commit_file("source.txt", "base\n", "Copy source");
        let copy_registry = RepositoryRegistry::default();
        let copy_opened = copy_registry
            .open(copy_repository.root.clone())
            .expect("open copy repository");
        copy_repository.write("source.txt", "base\nnew\n");
        copy_repository.write("copy.txt", "base\nnew\n");
        copy_repository.git_ok(["add", "--", "source.txt", "copy.txt"]);
        let changes = copy_registry
            .repository_changes(&copy_opened.repository_id)
            .expect("read copy");
        let copied = changes
            .files
            .iter()
            .find(|file| file.path.text == "copy.txt")
            .expect("copy entry");
        assert_eq!(
            copied.staged.as_ref().map(|facet| facet.kind),
            Some(ChangeKind::Copied)
        );
        let diff = copy_registry
            .file_diff(
                &copy_opened.repository_id,
                changes.change_set_id.as_str(),
                copied.file_id.as_str(),
                DiffSide::Staged,
            )
            .expect("copy diff");
        let FileDiffContent::Text { metadata, .. } = diff.content else {
            panic!("copy should be text metadata");
        };
        assert!(metadata.copied, "copy metadata: {metadata:?}");
    }

    #[cfg(unix)]
    #[test]
    fn selected_diff_handles_untracked_unusual_paths_and_symlinks() {
        use std::{
            ffi::OsString,
            os::unix::{ffi::OsStringExt, fs::symlink},
        };

        let repository = TestRepository::new();
        repository.commit_file("base.txt", "base\n", "Base");
        repository.write("-leading\tline\n文件.txt", "odd\n");
        fs::write(
            repository
                .root
                .join(OsString::from_vec(b"invalid-\xff.txt".to_vec())),
            b"invalid path\n",
        )
        .expect("write invalid-byte path");
        symlink("/etc/passwd", repository.root.join("link")).expect("create untracked symlink");
        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(repository.root.clone())
            .expect("open repository");
        let changes = registry
            .repository_changes(&opened.repository_id)
            .expect("read unusual changes");

        for display in ["-leading\\tline\\n文件.txt", "invalid-\\xFF.txt", "link"] {
            let file = changes
                .files
                .iter()
                .find(|file| file.path.text == display)
                .expect("unusual entry");
            let diff = registry
                .file_diff(
                    &opened.repository_id,
                    changes.change_set_id.as_str(),
                    file.file_id.as_str(),
                    DiffSide::Unstaged,
                )
                .expect("untracked diff");
            if display == "link" {
                let FileDiffContent::Text { hunks, .. } = diff.content else {
                    panic!("symlink should be represented as text");
                };
                let additions = hunks
                    .iter()
                    .flat_map(|hunk| &hunk.lines)
                    .filter(|line| line.kind == DiffLineKind::Addition)
                    .map(|line| line.content.as_str())
                    .collect::<Vec<_>>();
                assert_eq!(additions, ["/etc/passwd"]);
            } else {
                assert!(matches!(diff.content, FileDiffContent::Text { .. }));
            }
        }

        let leading = changes
            .files
            .iter()
            .find(|file| file.path.text == "-leading\\tline\\n文件.txt")
            .expect("leading-dash entry");
        let unavailable = registry
            .file_diff(
                &opened.repository_id,
                changes.change_set_id.as_str(),
                leading.file_id.as_str(),
                DiffSide::Staged,
            )
            .expect("closed-side response");
        assert_eq!(
            unavailable.content,
            FileDiffContent::Unavailable {
                reason: DiffUnavailableReason::SideUnavailable
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn selected_diff_treats_tracked_pathspec_syntax_as_a_literal_filename() {
        let repository = TestRepository::new();
        repository.write("*.txt", "literal base\n");
        repository.write("other.txt", "other base\n");
        repository.git_ok(["add", "--all"]);
        repository.git_ok(["commit", "-m", "Base"]);
        repository.write("*.txt", "literal changed\n");
        repository.write("other.txt", "other changed\n");

        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(repository.root.clone())
            .expect("open pathspec repository");
        let changes = registry
            .repository_changes(&opened.repository_id)
            .expect("read pathspec changes");
        let literal = changes
            .files
            .iter()
            .find(|file| file.path.text == "*.txt")
            .expect("literal pathspec filename");
        let diff = registry
            .file_diff(
                &opened.repository_id,
                changes.change_set_id.as_str(),
                literal.file_id.as_str(),
                DiffSide::Unstaged,
            )
            .expect("read literal pathspec diff");

        let FileDiffContent::Text {
            additions,
            deletions,
            hunks,
            ..
        } = diff.content
        else {
            panic!("expected a text diff for the literal pathspec filename");
        };
        assert_eq!((additions, deletions), (1, 1));
        let changed_lines = hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .filter(|line| line.kind != DiffLineKind::Context)
            .map(|line| line.content.as_str())
            .collect::<Vec<_>>();
        assert_eq!(changed_lines, ["literal base", "literal changed"]);
    }

    #[test]
    fn selected_diff_neutralizes_suppressed_blank_context_formatting() {
        let repository = TestRepository::new();
        repository.commit_file("blank.txt", "alpha\n\nmiddle\n\nomega\n", "Base");
        repository.git_ok(["config", "diff.suppressBlankEmpty", "true"]);
        repository.write("blank.txt", "alpha\n\nchanged middle\n\nomega\n");

        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(repository.root.clone())
            .expect("open blank-context repository");
        let changes = registry
            .repository_changes(&opened.repository_id)
            .expect("read blank-context changes");
        let blank = changes
            .files
            .iter()
            .find(|file| file.path.text == "blank.txt")
            .expect("blank-context file");
        let diff = registry
            .file_diff(
                &opened.repository_id,
                changes.change_set_id.as_str(),
                blank.file_id.as_str(),
                DiffSide::Unstaged,
            )
            .expect("read deterministic blank-context diff");

        let FileDiffContent::Text { hunks, .. } = diff.content else {
            panic!("expected a text diff with blank context");
        };
        assert_eq!(hunks.len(), 1);
        let hunk = &hunks[0];
        assert_eq!((hunk.old_start, hunk.old_count), (1, 5));
        assert_eq!((hunk.new_start, hunk.new_count), (1, 5));
        let blank_context = hunk
            .lines
            .iter()
            .filter(|line| line.kind == DiffLineKind::Context && line.content.is_empty())
            .count();
        assert_eq!(blank_context, 2);
    }

    #[test]
    fn selected_diff_classifies_binary_and_unsupported_text_encoding() {
        let binary_repository = TestRepository::new();
        binary_repository.commit_file("binary.dat", "base\n", "Base");
        fs::write(binary_repository.root.join("binary.dat"), b"next\0bytes")
            .expect("write binary file");
        let binary_registry = RepositoryRegistry::default();
        let opened = binary_registry
            .open(binary_repository.root.clone())
            .expect("open binary repository");
        let changes = binary_registry
            .repository_changes(&opened.repository_id)
            .expect("read binary change");
        let diff = binary_registry
            .file_diff(
                &opened.repository_id,
                changes.change_set_id.as_str(),
                changes.files[0].file_id.as_str(),
                DiffSide::Unstaged,
            )
            .expect("binary diff");
        assert_eq!(diff.content, FileDiffContent::Binary);

        let encoding_repository = TestRepository::new();
        encoding_repository.commit_file("text.txt", "base\n", "Base");
        fs::write(encoding_repository.root.join("text.txt"), b"invalid \xff\n")
            .expect("write invalid UTF-8 text");
        let encoding_registry = RepositoryRegistry::default();
        let opened = encoding_registry
            .open(encoding_repository.root.clone())
            .expect("open encoding repository");
        let changes = encoding_registry
            .repository_changes(&opened.repository_id)
            .expect("read encoding change");
        let diff = encoding_registry
            .file_diff(
                &opened.repository_id,
                changes.change_set_id.as_str(),
                changes.files[0].file_id.as_str(),
                DiffSide::Unstaged,
            )
            .expect("unsupported encoding response");
        assert_eq!(
            diff.content,
            FileDiffContent::Unavailable {
                reason: DiffUnavailableReason::UnsupportedEncoding
            }
        );
    }

    #[test]
    fn selected_diff_returns_a_typed_too_large_state() {
        let repository = TestRepository::new();
        repository.commit_file("large.txt", "base\n", "Base");
        fs::write(
            repository.root.join("large.txt"),
            vec![b'x'; 5 * 1024 * 1024],
        )
        .expect("write oversized text diff");
        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(repository.root.clone())
            .expect("open large repository");
        let changes = registry
            .repository_changes(&opened.repository_id)
            .expect("read large change");
        let diff = registry
            .file_diff(
                &opened.repository_id,
                changes.change_set_id.as_str(),
                changes.files[0].file_id.as_str(),
                DiffSide::Unstaged,
            )
            .expect("large diff response");

        assert_eq!(diff.content, FileDiffContent::TooLarge);
    }

    #[test]
    fn selected_diff_returns_conflict_submodule_and_stale_states() {
        let conflict_repository = TestRepository::new();
        conflict_repository.commit_file("conflict.txt", "base\n", "Base");
        conflict_repository.git_ok(["checkout", "-b", "feature"]);
        conflict_repository.commit_file("conflict.txt", "feature\n", "Feature");
        conflict_repository.git_ok(["checkout", "main"]);
        conflict_repository.commit_file("conflict.txt", "main\n", "Main");
        conflict_repository.git_failure(["merge", "feature"]);
        let conflict_registry = RepositoryRegistry::default();
        let opened = conflict_registry
            .open(conflict_repository.root.clone())
            .expect("open conflicted repository");
        let changes = conflict_registry
            .repository_changes(&opened.repository_id)
            .expect("read conflict");
        let diff = conflict_registry
            .file_diff(
                &opened.repository_id,
                changes.change_set_id.as_str(),
                changes.files[0].file_id.as_str(),
                DiffSide::Unstaged,
            )
            .expect("conflict response");
        assert!(matches!(diff.content, FileDiffContent::Conflict { .. }));

        let submodule_repository = TestRepository::new();
        submodule_repository.commit_file("base.txt", "base\n", "Base");
        let gitlink_oid = submodule_repository.oid("HEAD");
        submodule_repository.git_ok([
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{gitlink_oid},nested"),
        ]);
        let submodule_registry = RepositoryRegistry::default();
        let opened = submodule_registry
            .open(submodule_repository.root.clone())
            .expect("open gitlink repository");
        let changes = submodule_registry
            .repository_changes(&opened.repository_id)
            .expect("read gitlink");
        let nested = changes
            .files
            .iter()
            .find(|file| file.path.text == "nested")
            .expect("gitlink entry");
        let diff = submodule_registry
            .file_diff(
                &opened.repository_id,
                changes.change_set_id.as_str(),
                nested.file_id.as_str(),
                DiffSide::Staged,
            )
            .expect("submodule response");
        assert!(matches!(diff.content, FileDiffContent::Submodule { .. }));

        let stale_repository = TestRepository::new();
        stale_repository.commit_file("tracked.txt", "base\n", "Base");
        stale_repository.write("tracked.txt", "changed\n");
        let stale_registry = RepositoryRegistry::default();
        let opened = stale_registry
            .open(stale_repository.root.clone())
            .expect("open stale repository");
        let changes = stale_registry
            .repository_changes(&opened.repository_id)
            .expect("read stale fixture");
        stale_repository.write("tracked.txt", "base\n");
        let diff = stale_registry
            .file_diff(
                &opened.repository_id,
                changes.change_set_id.as_str(),
                changes.files[0].file_id.as_str(),
                DiffSide::Unstaged,
            )
            .expect("stale response");
        assert_eq!(
            diff.content,
            FileDiffContent::Unavailable {
                reason: DiffUnavailableReason::Stale
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn selected_diff_does_not_execute_filter_textconv_external_diff_or_fsmonitor() {
        use std::os::unix::fs::PermissionsExt;

        let repository = TestRepository::new();
        repository.write(
            ".gitattributes",
            "tracked.txt filter=orbit-filter diff=orbit-textconv\n",
        );
        repository.write("tracked.txt", "base\n");
        repository.git_ok(["add", "--", ".gitattributes", "tracked.txt"]);
        repository.git_ok(["commit", "-m", "Base"]);

        let filter_marker = repository.root.join("filter-ran");
        let textconv_marker = repository.root.join("textconv-ran");
        let external_marker = repository.root.join("external-diff-ran");
        let fsmonitor_marker = repository.root.join("fsmonitor-ran");
        for (name, marker) in [
            ("filter-helper", &filter_marker),
            ("textconv-helper", &textconv_marker),
            ("external-helper", &external_marker),
            ("fsmonitor-helper", &fsmonitor_marker),
        ] {
            let helper = repository.root.join(name);
            repository.write(
                name,
                &format!("#!/bin/sh\n: > '{}'\nexit 0\n", marker.display()),
            );
            let mut permissions = fs::metadata(&helper)
                .expect("helper metadata")
                .permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(helper, permissions).expect("make helper executable");
        }
        let filter = repository.root.join("filter-helper");
        let textconv = repository.root.join("textconv-helper");
        let external = repository.root.join("external-helper");
        let fsmonitor = repository.root.join("fsmonitor-helper");
        repository.git_ok([
            "config",
            "filter.orbit-filter.clean",
            filter.to_str().expect("UTF-8 helper path"),
        ]);
        repository.git_ok([
            "config",
            "filter.orbit-filter.process",
            filter.to_str().expect("UTF-8 helper path"),
        ]);
        repository.git_ok(["config", "filter.orbit-filter.required", "true"]);
        repository.git_ok([
            "config",
            "diff.orbit-textconv.textconv",
            textconv.to_str().expect("UTF-8 helper path"),
        ]);
        repository.git_ok([
            "config",
            "diff.external",
            external.to_str().expect("UTF-8 helper path"),
        ]);
        repository.git_ok([
            "config",
            "core.fsmonitor",
            fsmonitor.to_str().expect("UTF-8 helper path"),
        ]);
        repository.write("tracked.txt", "changed\n");

        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(repository.root.clone())
            .expect("open protected repository");
        let changes = registry
            .repository_changes(&opened.repository_id)
            .expect("read protected changes");
        let tracked = changes
            .files
            .iter()
            .find(|file| file.path.text == "tracked.txt")
            .expect("tracked change");
        let diff = registry
            .file_diff(
                &opened.repository_id,
                changes.change_set_id.as_str(),
                tracked.file_id.as_str(),
                DiffSide::Unstaged,
            )
            .expect("read protected diff");
        assert!(matches!(diff.content, FileDiffContent::Text { .. }));
        for marker in [
            filter_marker,
            textconv_marker,
            external_marker,
            fsmonitor_marker,
        ] {
            assert!(!marker.exists(), "configured helper must not execute");
        }
    }

    #[cfg(unix)]
    #[test]
    fn crafted_filter_name_fails_status_and_diff_closed_without_execution() {
        use std::os::unix::fs::PermissionsExt;

        let repository = TestRepository::new();
        repository.write(".gitattributes", "tracked.txt filter=a=b\n");
        repository.write("tracked.txt", "base\n");
        repository.git_ok(["add", "--", ".gitattributes", "tracked.txt"]);
        repository.git_ok(["commit", "-m", "Base"]);
        repository.write("tracked.txt", "changed\n");

        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(repository.root.clone())
            .expect("open repository before crafted filter configuration");
        let changes = registry
            .repository_changes(&opened.repository_id)
            .expect("read change set before crafted filter configuration");
        let tracked = changes
            .files
            .iter()
            .find(|file| file.path.text == "tracked.txt")
            .expect("tracked change");

        let marker = repository.root.join("crafted-filter-ran");
        let helper = repository.root.join("crafted-filter-helper");
        repository.write(
            "crafted-filter-helper",
            &format!("#!/bin/sh\n: > '{}'\ncat\n", marker.display()),
        );
        let mut permissions = fs::metadata(&helper)
            .expect("helper metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&helper, permissions).expect("make helper executable");
        repository.git_ok([
            "config",
            "filter.a=b.clean",
            helper.to_str().expect("UTF-8 helper path"),
        ]);
        repository.git_ok(["config", "filter.a=b.required", "true"]);

        let status_error = registry
            .repository_changes(&opened.repository_id)
            .expect_err("crafted filter name must make detailed status fail closed");
        assert_eq!(status_error.code, "unsupported_repository_state");
        assert_eq!(status_error.operation, "read_git_configuration");
        assert!(
            !marker.exists(),
            "status must not execute the crafted filter"
        );

        let diff_error = registry
            .file_diff(
                &opened.repository_id,
                changes.change_set_id.as_str(),
                tracked.file_id.as_str(),
                DiffSide::Unstaged,
            )
            .expect_err("crafted filter name must make selected diff fail closed");
        assert_eq!(diff_error.code, "unsupported_repository_state");
        assert_eq!(diff_error.operation, "read_git_configuration");
        assert!(!marker.exists(), "diff must not execute the crafted filter");
    }

    #[cfg(unix)]
    #[test]
    fn m2_reads_do_not_execute_configured_pager_or_hooks() {
        use std::os::unix::fs::PermissionsExt;

        let repository = TestRepository::new();
        repository.commit_file("tracked.txt", "base\n", "Base");

        let pager_marker = repository.root.join("pager-ran");
        let hook_marker = repository.root.join("hook-ran");
        let pager = repository.root.join("pager-helper");
        repository.write(
            "pager-helper",
            &format!("#!/bin/sh\n: > '{}'\ncat\n", pager_marker.display()),
        );
        let mut permissions = fs::metadata(&pager).expect("pager metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&pager, permissions).expect("make pager executable");

        let hooks = repository.root.join("hooks");
        fs::create_dir(&hooks).expect("create hooks directory");
        for name in ["pre-commit", "post-index-change", "reference-transaction"] {
            let hook = hooks.join(name);
            fs::write(
                &hook,
                format!("#!/bin/sh\n: > '{}'\nexit 0\n", hook_marker.display()),
            )
            .expect("write hook fixture");
            let mut permissions = fs::metadata(&hook).expect("hook metadata").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&hook, permissions).expect("make hook executable");
        }

        repository.git_ok([
            "config",
            "core.pager",
            pager.to_str().expect("UTF-8 pager path"),
        ]);
        repository.git_ok(["config", "pager.status", "true"]);
        repository.git_ok(["config", "pager.diff", "true"]);
        repository.git_ok([
            "config",
            "core.hooksPath",
            hooks.to_str().expect("UTF-8 hooks path"),
        ]);
        repository.write("tracked.txt", "changed\n");

        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(repository.root.clone())
            .expect("open protected repository");
        let changes = registry
            .repository_changes(&opened.repository_id)
            .expect("read protected changes");
        let tracked = changes
            .files
            .iter()
            .find(|file| file.path.text == "tracked.txt")
            .expect("tracked change");
        let diff = registry
            .file_diff(
                &opened.repository_id,
                changes.change_set_id.as_str(),
                tracked.file_id.as_str(),
                DiffSide::Unstaged,
            )
            .expect("read protected diff");

        assert!(matches!(diff.content, FileDiffContent::Text { .. }));
        assert!(!pager_marker.exists(), "configured pager must not execute");
        assert!(!hook_marker.exists(), "configured hooks must not execute");
    }

    #[cfg(unix)]
    #[test]
    fn selected_diff_does_not_lazy_fetch_a_missing_promised_blob() {
        use std::os::unix::fs::PermissionsExt;

        let repository = TestRepository::new();
        repository.commit_file("tracked.txt", "base\n", "Base");
        let blob_oid = repository.git_output(["rev-parse", "HEAD:tracked.txt"]);
        let object_path = repository
            .root
            .join(".git/objects")
            .join(&blob_oid[..2])
            .join(&blob_oid[2..]);
        assert!(object_path.is_file(), "fixture blob should be loose");

        let marker = repository.root.join("diff-lazy-fetch-ran");
        let remote = repository.root.join("fake-diff-promisor-remote");
        repository.write(
            "fake-diff-promisor-remote",
            &format!("#!/bin/sh\n: > '{}'\nexit 1\n", marker.display()),
        );
        let mut permissions = fs::metadata(&remote)
            .expect("remote metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&remote, permissions).expect("make remote executable");
        repository.git_ok(["config", "core.repositoryformatversion", "1"]);
        repository.git_ok(["config", "extensions.partialClone", "origin"]);
        repository.git_ok(["config", "remote.origin.promisor", "true"]);
        repository.git_ok(["config", "remote.origin.partialclonefilter", "blob:none"]);
        repository.git_ok([
            "config",
            "remote.origin.url",
            &format!("ext::{}", remote.display()),
        ]);
        repository.git_ok(["config", "protocol.ext.allow", "always"]);
        fs::remove_file(&object_path).expect("remove promised blob");
        repository.write("tracked.txt", "changed\n");

        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(repository.root.clone())
            .expect("open partial-clone fixture");
        let changes = registry
            .repository_changes(&opened.repository_id)
            .expect("read changes without fetching blob");
        let tracked = changes
            .files
            .iter()
            .find(|file| file.path.text == "tracked.txt")
            .expect("tracked change");
        let error = registry
            .file_diff(
                &opened.repository_id,
                changes.change_set_id.as_str(),
                tracked.file_id.as_str(),
                DiffSide::Unstaged,
            )
            .expect_err("missing blob should fail closed");

        assert_eq!(error.code, "git_command_failed");
        assert!(!marker.exists(), "promisor remote must not execute");
    }

    #[cfg(unix)]
    #[test]
    fn stages_supported_file_facets_and_installs_fresh_change_sets() {
        use std::os::unix::fs::symlink;

        let repository = TestRepository::new();
        for path in ["mixed.txt", "deleted.txt", "type.txt"] {
            repository.write(path, "base\n");
        }
        repository.git_ok(["add", "--all"]);
        repository.git_ok(["commit", "-m", "Base"]);

        repository.write("mixed.txt", "staged\n");
        repository.git_ok(["add", "--", "mixed.txt"]);
        repository.write("mixed.txt", "staged and unstaged\n");
        fs::remove_file(repository.root.join("deleted.txt")).expect("delete tracked fixture");
        fs::remove_file(repository.root.join("type.txt")).expect("replace tracked fixture");
        symlink("target", repository.root.join("type.txt")).expect("create type-change symlink");
        repository.write("untracked.txt", "new\n");
        symlink("target", repository.root.join("new-link")).expect("create new symlink");

        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(repository.root.clone())
            .expect("open fixture");
        let mut changes = registry
            .repository_changes(&opened.repository_id)
            .expect("initial detailed changes");
        let original_change_set = changes.change_set_id.as_str().to_owned();
        let original_file = changed_file(&changes, "mixed.txt")
            .file_id
            .as_str()
            .to_owned();

        for path in [
            "mixed.txt",
            "deleted.txt",
            "type.txt",
            "untracked.txt",
            "new-link",
        ] {
            let file = changed_file(&changes, path);
            let receipt = registry
                .stage_file(
                    &opened.repository_id,
                    changes.change_set_id.as_str(),
                    file.file_id.as_str(),
                )
                .unwrap_or_else(|error| panic!("stage {path:?}: {error:?}"));
            assert_eq!(receipt.outcome, MutationOutcome::Applied, "stage {path:?}");
            assert!(!receipt.refresh_required);
            changes = receipt
                .repository_changes
                .expect("successful stage installs detailed changes");
        }

        assert_eq!(changes.summary.unstaged, 0);
        assert_eq!(changes.summary.untracked, 0);
        assert_eq!(changes.summary.staged, 5);
        let stale = registry
            .stage_file(&opened.repository_id, &original_change_set, &original_file)
            .expect_err("pre-mutation handles must remain retired");
        assert_eq!(stale.code, "change_set_unavailable");
    }

    #[test]
    fn literal_stage_file_supports_option_pathspec_and_control_names() {
        let repository = TestRepository::new();
        let names = [
            "*.txt",
            ":(glob)magic.txt",
            "-leading.txt",
            "space name.txt",
            "unicodé.txt",
            "tab\tname.txt",
            "line\nbreak.txt",
        ];
        for name in names {
            repository.write(name, "selected\n");
        }
        repository.write("other.txt", "must remain untracked\n");

        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(repository.root.clone())
            .expect("open fixture");
        let mut changes = registry
            .repository_changes(&opened.repository_id)
            .expect("read unusual changes");
        let displays = [
            "*.txt",
            ":(glob)magic.txt",
            "-leading.txt",
            "space name.txt",
            "unicodé.txt",
            "tab\\tname.txt",
            "line\\nbreak.txt",
        ];

        for display in displays {
            let file = changed_file(&changes, display);
            let receipt = registry
                .stage_file(
                    &opened.repository_id,
                    changes.change_set_id.as_str(),
                    file.file_id.as_str(),
                )
                .unwrap_or_else(|error| panic!("stage {display:?}: {error:?}"));
            assert_eq!(receipt.outcome, MutationOutcome::Applied);
            changes = receipt.repository_changes.expect("fresh change set");
        }

        let other = changed_file(&changes, "other.txt");
        assert_eq!(
            other.unstaged.as_ref().map(|facet| facet.kind),
            Some(ChangeKind::Untracked)
        );
        assert!(other.staged.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn stage_file_preserves_invalid_utf8_path_authority_inside_rust() {
        let repository = TestRepository::new();
        let raw = b"invalid-\xff.txt".to_vec();
        let path = PathBuf::from(OsString::from_vec(raw.clone()));
        repository.write_path(&path, b"content\n");

        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(repository.root.clone())
            .expect("open fixture");
        let changes = registry
            .repository_changes(&opened.repository_id)
            .expect("read byte path");
        let file = changes
            .files
            .iter()
            .find(|file| file.path.text == "invalid-\\xFF.txt")
            .expect("escaped byte path");
        let receipt = registry
            .stage_file(
                &opened.repository_id,
                changes.change_set_id.as_str(),
                file.file_id.as_str(),
            )
            .expect("stage invalid UTF-8 path");
        assert_eq!(receipt.outcome, MutationOutcome::Applied);

        let listed = repository.git_bytes([OsString::from("ls-files"), OsString::from("-z")]);
        let mut expected = raw;
        expected.push(0);
        assert_eq!(listed, expected);
    }

    #[test]
    fn born_and_unborn_unstage_file_and_all_preserve_worktree_content() {
        let born = TestRepository::new();
        born.commit_file("tracked.txt", "base\n", "Base");
        born.write("tracked.txt", "staged\n");
        born.git_ok(["add", "--", "tracked.txt"]);
        born.write("tracked.txt", "later worktree edit\n");
        born.write("added.txt", "added\n");
        born.git_ok(["add", "--", "added.txt"]);
        let born_registry = RepositoryRegistry::default();
        let opened = born_registry
            .open(born.root.clone())
            .expect("open born fixture");
        let changes = born_registry
            .repository_changes(&opened.repository_id)
            .expect("born changes");
        let tracked = changed_file(&changes, "tracked.txt");
        let receipt = born_registry
            .unstage_file(
                &opened.repository_id,
                changes.change_set_id.as_str(),
                tracked.file_id.as_str(),
            )
            .expect("unstage mixed tracked file");
        assert_eq!(receipt.outcome, MutationOutcome::Applied);
        assert_eq!(
            fs::read_to_string(born.root.join("tracked.txt")).expect("worktree content"),
            "later worktree edit\n"
        );
        let changes = receipt.repository_changes.expect("fresh born changes");
        let receipt = born_registry
            .unstage_all(&opened.repository_id, changes.change_set_id.as_str())
            .expect("unstage all born");
        assert_eq!(receipt.outcome, MutationOutcome::Applied);
        let changes = receipt.repository_changes.expect("fresh born changes");
        assert_eq!(changes.summary.staged, 0);
        assert!(born.root.join("added.txt").exists());

        let unborn = TestRepository::new();
        unborn.write("one.txt", "one\n");
        unborn.write("two.txt", "two\n");
        unborn.git_ok(["add", "--all"]);
        let unborn_registry = RepositoryRegistry::default();
        let opened = unborn_registry
            .open(unborn.root.clone())
            .expect("open unborn fixture");
        let changes = unborn_registry
            .repository_changes(&opened.repository_id)
            .expect("unborn changes");
        let one = changed_file(&changes, "one.txt");
        let receipt = unborn_registry
            .unstage_file(
                &opened.repository_id,
                changes.change_set_id.as_str(),
                one.file_id.as_str(),
            )
            .expect("unborn unstage file");
        assert_eq!(receipt.outcome, MutationOutcome::Applied);
        assert!(unborn.root.join("one.txt").exists());
        let changes = receipt.repository_changes.expect("fresh unborn changes");
        let receipt = unborn_registry
            .unstage_all(&opened.repository_id, changes.change_set_id.as_str())
            .expect("unborn unstage all");
        assert_eq!(receipt.outcome, MutationOutcome::Applied);
        assert_eq!(
            receipt
                .repository_changes
                .expect("fresh empty index")
                .summary
                .staged,
            0
        );
        assert!(unborn.root.join("two.txt").exists());
    }

    #[test]
    fn stage_all_includes_add_modify_delete_and_rejects_stale_structure() {
        let repository = TestRepository::new();
        repository.commit_file("modified.txt", "base\n", "Base");
        repository.write("deleted.txt", "base\n");
        repository.git_ok(["add", "--", "deleted.txt"]);
        repository.git_ok(["commit", "-m", "Second base"]);
        repository.write("modified.txt", "changed\n");
        fs::remove_file(repository.root.join("deleted.txt")).expect("delete fixture");
        repository.write("new.txt", "new\n");

        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(repository.root.clone())
            .expect("open fixture");
        let stale = registry
            .repository_changes(&opened.repository_id)
            .expect("stale candidate");
        repository.write("appeared-later.txt", "late\n");
        let error = registry
            .stage_all(&opened.repository_id, stale.change_set_id.as_str())
            .expect_err("structural change must reject stale stage all");
        assert_eq!(error.code, "mutation_stale");

        let current = registry
            .repository_changes(&opened.repository_id)
            .expect("fresh changes");
        let receipt = registry
            .stage_all(&opened.repository_id, current.change_set_id.as_str())
            .expect("stage all");
        assert_eq!(receipt.outcome, MutationOutcome::Applied);
        let changes = receipt.repository_changes.expect("fresh staged state");
        assert_eq!(changes.summary.unstaged, 0);
        assert_eq!(changes.summary.untracked, 0);
        assert_eq!(changes.summary.staged, 4);
    }

    #[test]
    fn file_mutation_rejects_external_structure_changes_before_start() {
        let repository = TestRepository::new();
        repository.commit_file("tracked.txt", "base\n", "Base");
        repository.write("tracked.txt", "changed\n");
        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(repository.root.clone())
            .expect("open fixture");
        let changes = registry
            .repository_changes(&opened.repository_id)
            .expect("read changes");
        let file = changed_file(&changes, "tracked.txt");
        fs::rename(
            repository.root.join("tracked.txt"),
            repository.root.join("renamed.txt"),
        )
        .expect("rename outside Orbit");

        let error = registry
            .stage_file(
                &opened.repository_id,
                changes.change_set_id.as_str(),
                file.file_id.as_str(),
            )
            .expect_err("structurally stale file must reject");
        assert_eq!(error.code, "mutation_stale");
        registry
            .changes
            .authorize_for_test(
                &opened.repository_id,
                &repository.root,
                changes.change_set_id.as_str(),
                file.file_id.as_str(),
            )
            .expect("pre-start rejection must preserve old authority");
    }

    #[test]
    fn file_mutation_rejects_head_movement_even_when_its_facet_is_unchanged() {
        let repository = TestRepository::new();
        repository.commit_file("tracked.txt", "base\n", "Base");
        repository.write("tracked.txt", "changed\n");
        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(repository.root.clone())
            .expect("open fixture");
        let changes = registry
            .repository_changes(&opened.repository_id)
            .expect("read changes");
        let file = changed_file(&changes, "tracked.txt");
        repository.write("external.txt", "external\n");
        repository.git_ok(["add", "--", "external.txt"]);
        repository.git_ok(["commit", "-m", "External commit"]);

        let error = registry
            .stage_file(
                &opened.repository_id,
                changes.change_set_id.as_str(),
                file.file_id.as_str(),
            )
            .expect_err("HEAD movement must invalidate mutation preflight");
        assert_eq!(error.code, "mutation_stale");
    }

    #[test]
    fn stage_all_supports_an_unborn_repository() {
        let repository = TestRepository::new();
        repository.write("first.txt", "first\n");
        repository.write("second.txt", "second\n");
        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(repository.root.clone())
            .expect("open unborn fixture");
        let changes = registry
            .repository_changes(&opened.repository_id)
            .expect("unborn changes");
        let receipt = registry
            .stage_all(&opened.repository_id, changes.change_set_id.as_str())
            .expect("stage all unborn");
        assert_eq!(receipt.outcome, MutationOutcome::Applied);
        let post = receipt.repository_changes.expect("fresh unborn state");
        assert_eq!(post.summary.staged, 2);
        assert_eq!(post.summary.untracked, 0);
        assert!(post.head.oid.is_none());
    }

    #[test]
    fn unstage_rename_uses_both_endpoints_and_copy_keeps_its_source_staged() {
        let rename = TestRepository::new();
        rename.commit_file("old.txt", "base\n", "Base");
        rename.git_ok(["mv", "old.txt", "new.txt"]);
        let rename_registry = RepositoryRegistry::default();
        let opened = rename_registry
            .open(rename.root.clone())
            .expect("open rename");
        let changes = rename_registry
            .repository_changes(&opened.repository_id)
            .expect("rename changes");
        let target = changed_file(&changes, "new.txt");
        assert_eq!(
            target.staged.as_ref().map(|facet| facet.kind),
            Some(ChangeKind::Renamed)
        );
        let receipt = rename_registry
            .unstage_file(
                &opened.repository_id,
                changes.change_set_id.as_str(),
                target.file_id.as_str(),
            )
            .expect("unstage rename");
        assert_eq!(receipt.outcome, MutationOutcome::Applied);
        let post = receipt.repository_changes.expect("fresh rename state");
        assert_eq!(post.summary.staged, 0);
        assert!(rename.root.join("new.txt").exists());

        let copy = TestRepository::new();
        copy.commit_file("source.txt", "base\n", "Base");
        copy.write("source.txt", "source changed\n");
        copy.write("copy.txt", "base\n");
        copy.git_ok(["add", "--", "source.txt", "copy.txt"]);
        let copy_registry = RepositoryRegistry::default();
        let opened = copy_registry.open(copy.root.clone()).expect("open copy");
        let changes = copy_registry
            .repository_changes(&opened.repository_id)
            .expect("copy changes");
        let copied = changed_file(&changes, "copy.txt");
        assert_eq!(
            copied.staged.as_ref().map(|facet| facet.kind),
            Some(ChangeKind::Copied)
        );
        let receipt = copy_registry
            .unstage_file(
                &opened.repository_id,
                changes.change_set_id.as_str(),
                copied.file_id.as_str(),
            )
            .expect("unstage copy target");
        assert_eq!(receipt.outcome, MutationOutcome::Applied);
        let post = receipt.repository_changes.expect("fresh copy state");
        assert!(changed_file(&post, "source.txt").staged.is_some());
        assert!(changed_file(&post, "copy.txt").staged.is_none());
    }

    #[test]
    fn staging_a_submodule_updates_only_the_superproject_gitlink() {
        let source = TestRepository::new();
        source.commit_file("nested.txt", "base\n", "Nested base");
        let repository = TestRepository::new();
        repository.git_ok([
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            source.root.to_str().expect("UTF-8 source path"),
            "sub",
        ]);
        repository.git_ok(["commit", "-m", "Add submodule"]);
        let submodule = repository.root.join("sub");
        let run_in_submodule = |args: &[&str]| {
            let output = GitRunner::default()
                .run(Some(&submodule), "test_fixture", args, 1024 * 1024)
                .expect("run submodule Git");
            assert!(
                output.status.success(),
                "submodule Git failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        run_in_submodule(&["config", "user.name", "Orbit Tests"]);
        run_in_submodule(&["config", "user.email", "orbit@example.test"]);
        fs::write(submodule.join("nested.txt"), b"new commit\n").expect("write nested commit");
        run_in_submodule(&["add", "--", "nested.txt"]);
        run_in_submodule(&["commit", "-m", "Advance submodule"]);
        fs::write(submodule.join("nested.txt"), b"dirty after commit\n").expect("dirty submodule");

        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(repository.root.clone())
            .expect("open superproject");
        let changes = registry
            .repository_changes(&opened.repository_id)
            .expect("read gitlink change");
        let file = changed_file(&changes, "sub");
        assert!(file
            .submodule
            .as_ref()
            .is_some_and(|state| state.commit_changed));
        let receipt = registry
            .stage_file(
                &opened.repository_id,
                changes.change_set_id.as_str(),
                file.file_id.as_str(),
            )
            .expect("stage superproject gitlink");
        assert_eq!(receipt.outcome, MutationOutcome::Applied);
        let nested_status = GitRunner::default()
            .run(
                Some(&submodule),
                "test_fixture",
                ["status", "--porcelain"],
                1024,
            )
            .expect("read nested status");
        assert!(
            !nested_status.stdout.is_empty(),
            "nested dirty worktree must not be staged recursively"
        );
    }

    #[cfg(unix)]
    #[test]
    fn explicit_stage_runs_filters_and_index_hook_but_neutralizes_fsmonitor() {
        let repository = TestRepository::new();
        repository.write(".gitattributes", "tracked.txt filter=orbit-stage\n");
        repository.write("tracked.txt", "base\n");
        repository.git_ok(["add", "--", ".gitattributes", "tracked.txt"]);
        repository.git_ok(["commit", "-m", "Base"]);

        let filter_marker = repository.root.join("filter-ran");
        let fsmonitor_marker = repository.root.join("fsmonitor-ran");
        let hook_marker = repository.root.join("hook-ran");
        let filter = repository.root.join("filter.sh");
        fs::write(
            &filter,
            format!("#!/bin/sh\n: > '{}'\ncat\n", filter_marker.display()),
        )
        .expect("write filter");
        let fsmonitor = repository.root.join("fsmonitor.sh");
        fs::write(
            &fsmonitor,
            format!("#!/bin/sh\n: > '{}'\nexit 0\n", fsmonitor_marker.display()),
        )
        .expect("write fsmonitor");
        let hook = repository.root.join(".git/hooks/post-index-change");
        fs::write(
            &hook,
            format!("#!/bin/sh\n: > '{}'\n", hook_marker.display()),
        )
        .expect("write hook");
        for executable in [&filter, &fsmonitor, &hook] {
            let mut permissions = fs::metadata(executable).expect("metadata").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(executable, permissions).expect("make executable");
        }
        repository.git_ok([
            "config",
            "filter.orbit-stage.clean",
            filter.to_str().expect("UTF-8 filter path"),
        ]);
        repository.git_ok([
            "config",
            "core.fsmonitor",
            fsmonitor.to_str().expect("UTF-8 fsmonitor path"),
        ]);
        repository.write("tracked.txt", "changed\n");

        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(repository.root.clone())
            .expect("open fixture");
        let changes = registry
            .repository_changes(&opened.repository_id)
            .expect("read changes");
        assert!(!filter_marker.exists());
        assert!(!fsmonitor_marker.exists());
        assert!(!hook_marker.exists());
        let file = changed_file(&changes, "tracked.txt");
        let receipt = registry
            .stage_file(
                &opened.repository_id,
                changes.change_set_id.as_str(),
                file.file_id.as_str(),
            )
            .expect("stage with configured helpers");

        assert_eq!(receipt.outcome, MutationOutcome::Applied);
        assert!(
            filter_marker.exists(),
            "explicit stage should execute clean filter"
        );
        assert!(
            hook_marker.exists(),
            "explicit stage should execute index hook"
        );
        assert!(
            !fsmonitor_marker.exists(),
            "fsmonitor must remain neutralized"
        );
    }

    #[cfg(unix)]
    #[test]
    fn required_process_filter_rejection_is_typed_and_refreshes_state() {
        let repository = TestRepository::new();
        repository.write(".gitattributes", "tracked.txt filter=orbit-process\n");
        repository.write("tracked.txt", "base\n");
        repository.git_ok(["add", "--", ".gitattributes", "tracked.txt"]);
        repository.git_ok(["commit", "-m", "Base"]);
        let marker = repository.root.join(".git/process-filter-ran");
        let process = repository.root.join("process-filter.sh");
        fs::write(
            &process,
            format!(
                "#!/bin/sh\n: > '{}'\nprintf 'required process filter rejected\\n' >&2\nexit 1\n",
                marker.display()
            ),
        )
        .expect("write process filter");
        let mut permissions = fs::metadata(&process).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&process, permissions).expect("make process executable");
        repository.git_ok([
            "config",
            "filter.orbit-process.process",
            process.to_str().expect("UTF-8 process path"),
        ]);
        repository.git_ok(["config", "filter.orbit-process.required", "true"]);
        repository.write("tracked.txt", "changed\n");

        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(repository.root.clone())
            .expect("open fixture");
        let changes = registry
            .repository_changes(&opened.repository_id)
            .expect("read changes");
        let file = changed_file(&changes, "tracked.txt");
        let receipt = registry
            .stage_file(
                &opened.repository_id,
                changes.change_set_id.as_str(),
                file.file_id.as_str(),
            )
            .expect("started stage returns receipt");

        assert_eq!(receipt.outcome, MutationOutcome::Rejected);
        assert_eq!(
            receipt.issue.as_ref().map(|issue| issue.code),
            Some("mutation_rejected")
        );
        assert!(receipt
            .issue
            .as_ref()
            .and_then(|issue| issue.details.as_deref())
            .is_some_and(|detail| detail.contains("required process filter rejected")));
        assert!(receipt.repository_changes.is_some());
        assert!(!receipt.refresh_required);
        assert!(
            marker.exists(),
            "explicit mutation should invoke process filter"
        );
        let stale = registry
            .stage_file(
                &opened.repository_id,
                changes.change_set_id.as_str(),
                file.file_id.as_str(),
            )
            .expect_err("failed started mutation still retires old handles");
        assert_eq!(stale.code, "change_set_unavailable");
    }

    #[test]
    fn conflict_and_in_progress_operation_reject_before_handle_fencing() {
        let conflict = TestRepository::new();
        conflict.commit_file("conflict.txt", "base\n", "Base");
        conflict.git_ok(["checkout", "-b", "feature"]);
        conflict.commit_file("conflict.txt", "feature\n", "Feature");
        conflict.git_ok(["checkout", "main"]);
        conflict.commit_file("conflict.txt", "main\n", "Main");
        conflict.git_failure(["merge", "feature"]);
        let registry = RepositoryRegistry::default();
        let opened = registry.open(conflict.root.clone()).expect("open conflict");
        let changes = registry
            .repository_changes(&opened.repository_id)
            .expect("conflict changes");
        let error = registry
            .stage_all(&opened.repository_id, changes.change_set_id.as_str())
            .expect_err("conflict staging is deferred");
        assert_eq!(error.code, "mutation_not_applicable");
        let conflict_file = changed_file(&changes, "conflict.txt");
        registry
            .changes
            .authorize_for_test(
                &opened.repository_id,
                &conflict.root,
                changes.change_set_id.as_str(),
                conflict_file.file_id.as_str(),
            )
            .expect("preflight rejection keeps handles authoritative");

        let merge = TestRepository::new();
        merge.commit_file("base.txt", "base\n", "Base");
        merge.git_ok(["checkout", "-b", "feature"]);
        merge.commit_file("feature.txt", "feature\n", "Feature");
        merge.git_ok(["checkout", "main"]);
        merge.commit_file("main.txt", "main\n", "Main");
        merge.git_ok(["merge", "--no-commit", "feature"]);
        let registry = RepositoryRegistry::default();
        let opened = registry.open(merge.root.clone()).expect("open merge");
        let changes = registry
            .repository_changes(&opened.repository_id)
            .expect("merge changes");
        let error = registry
            .unstage_all(&opened.repository_id, changes.change_set_id.as_str())
            .expect_err("merge continuation is deferred");
        assert_eq!(error.code, "mutation_not_applicable");
    }

    #[test]
    fn every_fixed_pseudoref_and_git_state_path_blocks_staging() {
        let repository = TestRepository::new();
        repository.commit_file("tracked.txt", "base\n", "Base");
        repository.write("untracked.txt", "new\n");
        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(repository.root.clone())
            .expect("open fixture");
        let changes = registry
            .repository_changes(&opened.repository_id)
            .expect("read changes");
        let head = repository.oid("HEAD");

        for reference in [
            "MERGE_HEAD",
            "CHERRY_PICK_HEAD",
            "REVERT_HEAD",
            "REBASE_HEAD",
        ] {
            let path = repository.git_path(reference);
            fs::create_dir_all(path.parent().expect("pseudoref parent")).expect("create parent");
            fs::write(&path, format!("{head}\n")).expect("write pseudoref");
            let error = registry
                .stage_all(&opened.repository_id, changes.change_set_id.as_str())
                .expect_err("pseudoref must block staging");
            assert_eq!(error.code, "mutation_not_applicable", "{reference}");
            fs::remove_file(path).expect("remove pseudoref");
        }

        for state in ["rebase-merge", "rebase-apply", "sequencer", "SQUASH_MSG"] {
            let path = repository.git_path(state);
            if state == "SQUASH_MSG" {
                fs::create_dir_all(path.parent().expect("state parent")).expect("create parent");
                fs::write(&path, b"squash\n").expect("write state file");
            } else {
                fs::create_dir_all(&path).expect("create state directory");
            }
            let error = registry
                .stage_all(&opened.repository_id, changes.change_set_id.as_str())
                .expect_err("operation-state path must block staging");
            assert_eq!(error.code, "mutation_not_applicable", "{state}");
            if path.is_dir() {
                fs::remove_dir_all(path).expect("remove state directory");
            } else {
                fs::remove_file(path).expect("remove state file");
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn same_repository_concurrent_mutation_is_rejected_by_backend_lease() {
        let repository = TestRepository::new();
        repository.write(".gitattributes", "tracked.txt filter=orbit-blocking\n");
        repository.write("tracked.txt", "base\n");
        repository.git_ok(["add", "--", ".gitattributes", "tracked.txt"]);
        repository.git_ok(["commit", "-m", "Base"]);
        let entered = repository.root.join("filter-entered");
        let release = repository.root.join("filter-release");
        let filter = repository.root.join("blocking-filter.sh");
        fs::write(
            &filter,
            format!(
                "#!/bin/sh\n: > '{}'\nwhile [ ! -e '{}' ]; do sleep 0.01; done\ncat\n",
                entered.display(),
                release.display()
            ),
        )
        .expect("write blocking filter");
        let mut permissions = fs::metadata(&filter).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&filter, permissions).expect("make filter executable");
        repository.git_ok([
            "config",
            "filter.orbit-blocking.clean",
            filter.to_str().expect("UTF-8 filter path"),
        ]);
        repository.write("tracked.txt", "changed\n");

        let registry = Arc::new(RepositoryRegistry::default());
        let opened = registry
            .open(repository.root.clone())
            .expect("open fixture");
        let changes = registry
            .repository_changes(&opened.repository_id)
            .expect("read changes");
        let file_id = changed_file(&changes, "tracked.txt")
            .file_id
            .as_str()
            .to_owned();
        let repository_id = opened.repository_id.clone();
        let change_set_id = changes.change_set_id.as_str().to_owned();
        let worker_registry = Arc::clone(&registry);
        let worker_repository = repository_id.clone();
        let worker_change_set = change_set_id.clone();
        let worker_file = file_id.clone();
        let worker = thread::spawn(move || {
            worker_registry.stage_file(&worker_repository, &worker_change_set, &worker_file)
        });
        wait_for_path(&entered);

        let error = registry
            .stage_file(&repository_id, &change_set_id, &file_id)
            .expect_err("concurrent mutation should be busy");
        assert_eq!(error.code, "mutation_in_progress");
        fs::write(&release, b"continue\n").expect("release filter");
        let receipt = worker
            .join()
            .expect("mutation worker")
            .expect("first stage");
        assert_eq!(receipt.outcome, MutationOutcome::Applied);
    }

    #[cfg(unix)]
    #[test]
    fn post_index_timeout_is_uncertain_but_returns_the_actual_staged_state() {
        let repository = TestRepository::new();
        repository.commit_file("tracked.txt", "base\n", "Base");
        let entered = repository.root.join("hook-entered");
        let hook = repository.root.join(".git/hooks/post-index-change");
        fs::write(
            &hook,
            format!("#!/bin/sh\n: > '{}'\nsleep 30\n", entered.display()),
        )
        .expect("write slow index hook");
        let mut permissions = fs::metadata(&hook).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&hook, permissions).expect("make hook executable");
        repository.write("tracked.txt", "changed\n");

        let runner = GitRunner::default().with_mutation_deadline(Duration::from_millis(200));
        let registry = RepositoryRegistry::with_runner(runner);
        let opened = registry
            .open(repository.root.clone())
            .expect("open fixture");
        let changes = registry
            .repository_changes(&opened.repository_id)
            .expect("read changes");
        let file = changed_file(&changes, "tracked.txt");
        let receipt = registry
            .stage_file(
                &opened.repository_id,
                changes.change_set_id.as_str(),
                file.file_id.as_str(),
            )
            .expect("timed-out started mutation returns receipt");

        assert!(entered.exists());
        assert_eq!(receipt.outcome, MutationOutcome::Uncertain);
        assert_eq!(
            receipt.issue.as_ref().map(|issue| issue.code),
            Some("mutation_timed_out")
        );
        let post = receipt
            .repository_changes
            .expect("post-timeout status should still install");
        assert_eq!(post.summary.staged, 1);
        assert_eq!(post.summary.unstaged, 0);
        assert!(!receipt.refresh_required);
        let stale = registry
            .stage_file(
                &opened.repository_id,
                changes.change_set_id.as_str(),
                file.file_id.as_str(),
            )
            .expect_err("uncertain mutation must retire old handles");
        assert_eq!(stale.code, "change_set_unavailable");
    }

    #[cfg(unix)]
    #[test]
    fn unexpected_head_movement_from_index_hook_invalidates_history() {
        let repository = TestRepository::new();
        repository.commit_file("tracked.txt", "base\n", "Base");
        repository.commit_file("second.txt", "second\n", "Second");
        repository.write("tracked.txt", "changed\n");
        let hook = repository.root.join(".git/hooks/post-index-change");
        fs::write(
            &hook,
            format!(
                "#!/bin/sh\nrm -- '{}'\ngit commit --quiet -m 'Hook commit'\n",
                hook.display()
            ),
        )
        .expect("write committing hook");
        let mut permissions = fs::metadata(&hook).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&hook, permissions).expect("make hook executable");

        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(repository.root.clone())
            .expect("open fixture");
        let history = registry
            .commit_history_page(&opened.repository_id, None, Some(1))
            .expect("start history session");
        let cursor = history.next_cursor.expect("history cursor");
        let changes = registry
            .repository_changes(&opened.repository_id)
            .expect("read changes");
        let file = changed_file(&changes, "tracked.txt");
        let receipt = registry
            .stage_file(
                &opened.repository_id,
                changes.change_set_id.as_str(),
                file.file_id.as_str(),
            )
            .expect("stage with committing hook");

        assert_eq!(receipt.outcome, MutationOutcome::Uncertain);
        assert!(receipt.head_changed);
        let history_error = registry
            .commit_history_page(&opened.repository_id, Some(cursor.as_str()), Some(1))
            .expect_err("HEAD movement must invalidate old history sessions");
        assert_eq!(history_error.code, "history_session_unavailable");
    }

    #[cfg(unix)]
    #[test]
    fn forcing_add_ignore_errors_false_prevents_configured_partial_stage_all() {
        let repository = TestRepository::new();
        repository.write("readable.txt", "readable\n");
        repository.write("unreadable.txt", "unreadable\n");
        let unreadable = repository.root.join("unreadable.txt");
        let mut permissions = fs::metadata(&unreadable).expect("metadata").permissions();
        permissions.set_mode(0o000);
        fs::set_permissions(&unreadable, permissions).expect("make unreadable");
        repository.git_ok(["config", "add.ignoreErrors", "true"]);

        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(repository.root.clone())
            .expect("open fixture");
        let changes = registry
            .repository_changes(&opened.repository_id)
            .expect("read changes");
        let receipt = registry
            .stage_all(&opened.repository_id, changes.change_set_id.as_str())
            .expect("started stage all returns receipt");

        permissions = fs::metadata(&unreadable).expect("metadata").permissions();
        permissions.set_mode(0o644);
        fs::set_permissions(&unreadable, permissions).expect("restore permissions");
        assert_eq!(receipt.outcome, MutationOutcome::Rejected);
        let cached = repository.git_output(["diff", "--cached", "--name-only"]);
        assert!(
            cached.is_empty(),
            "fail-fast override must prevent partial staging"
        );
    }

    #[test]
    fn creates_born_and_unborn_normal_commits_from_the_authorized_index() {
        let born = TestRepository::new();
        born.commit_file("base.txt", "base\n", "base");
        born.write("next.txt", "next\n");
        born.git_ok(["add", "--", "next.txt"]);
        let registry = RepositoryRegistry::default();
        let opened = registry.open(born.root.clone()).expect("open born fixture");
        let changes = registry
            .repository_changes(&opened.repository_id)
            .expect("changes");
        let message = "Subject with -option\n\nUnicode β and a \\\\ slash\n";
        let receipt = registry
            .create_commit(
                &opened.repository_id,
                changes.change_set_id.as_str(),
                message,
            )
            .expect("commit receipt");
        assert_eq!(receipt.outcome, MutationOutcome::Applied);
        let head = born.oid("HEAD");
        assert_eq!(receipt.commit_oid.as_deref(), Some(head.as_str()));
        assert_eq!(
            born.git_output(["log", "-1", "--format=%B"]),
            message.trim_end()
        );
        assert_eq!(
            receipt
                .repository_changes
                .expect("post changes")
                .summary
                .staged,
            0
        );

        let unborn = TestRepository::new();
        unborn.write("first.txt", "first\n");
        unborn.git_ok(["add", "--", "first.txt"]);
        let opened = registry
            .open(unborn.root.clone())
            .expect("open unborn fixture");
        let changes = registry
            .repository_changes(&opened.repository_id)
            .expect("unborn changes");
        let receipt = registry
            .create_commit(
                &opened.repository_id,
                changes.change_set_id.as_str(),
                "first commit",
            )
            .expect("unborn commit receipt");
        assert_eq!(receipt.outcome, MutationOutcome::Applied);
        assert!(receipt.commit_oid.is_some());
    }

    #[test]
    fn rejects_invalid_commit_messages_and_empty_indexes_before_fencing_handles() {
        assert_eq!(
            validate_commit_message(" \n\t").expect_err("blank").code,
            "commit_message_invalid"
        );
        assert_eq!(
            validate_commit_message("a\0b").expect_err("nul").code,
            "commit_message_invalid"
        );
        assert_eq!(
            validate_commit_message(&"x".repeat(MAX_COMMIT_MESSAGE_BYTES + 1))
                .expect_err("limit")
                .code,
            "commit_message_invalid"
        );

        let repository = TestRepository::new();
        repository.commit_file("base.txt", "base\n", "base");
        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(repository.root.clone())
            .expect("open fixture");
        let changes = registry
            .repository_changes(&opened.repository_id)
            .expect("clean changes");
        let error = registry
            .create_commit(
                &opened.repository_id,
                changes.change_set_id.as_str(),
                "message",
            )
            .expect_err("empty index");
        assert_eq!(error.code, "commit_has_no_staged_changes");
        registry
            .changes
            .authorize_change_set(
                &opened.repository_id,
                &repository.root,
                changes.change_set_id.as_str(),
            )
            .expect("preflight rejection must retain handles");
    }

    #[cfg(unix)]
    #[test]
    fn commit_preserves_normal_hooks_and_reports_a_rejected_hook_without_moving_head() {
        let repository = TestRepository::new();
        repository.commit_file("base.txt", "base\n", "base");
        repository.write("next.txt", "next\n");
        repository.git_ok(["add", "--", "next.txt"]);
        let hooks = repository.root.join(".git/hooks");
        let pre_commit = hooks.join("pre-commit");
        fs::write(&pre_commit, "#!/bin/sh\necho hook-rejected >&2\nexit 1\n").expect("hook");
        let mut permissions = fs::metadata(&pre_commit).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&pre_commit, permissions).expect("executable hook");

        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(repository.root.clone())
            .expect("open fixture");
        let changes = registry
            .repository_changes(&opened.repository_id)
            .expect("changes");
        let before = repository.oid("HEAD");
        let receipt = registry
            .create_commit(
                &opened.repository_id,
                changes.change_set_id.as_str(),
                "message",
            )
            .expect("started commit returns receipt");
        assert_eq!(receipt.outcome, MutationOutcome::Rejected);
        assert_eq!(repository.oid("HEAD"), before);
        assert!(receipt
            .issue
            .and_then(|issue| issue.details)
            .is_some_and(|detail| detail.contains("hook-rejected")));
    }

    #[cfg(unix)]
    #[test]
    fn commit_uses_fixed_message_contract_and_preserves_message_and_post_hooks() {
        let repository = TestRepository::new();
        repository.commit_file("base.txt", "base\n", "base");
        repository.write("next.txt", "next\n");
        repository.git_ok(["add", "--", "next.txt"]);
        let hooks = repository.root.join(".git/hooks");
        let marker = repository.root.join("post-commit-ran");
        for (name, script) in [
            (
                "prepare-commit-msg",
                "#!/bin/sh\nprintf '\\nprepare hook' >> \"$1\"\n",
            ),
            (
                "commit-msg",
                "#!/bin/sh\nprintf '\\ncommit hook' >> \"$1\"\n",
            ),
            ("post-commit", "#!/bin/sh\n: > post-commit-ran\n"),
        ] {
            let hook = hooks.join(name);
            fs::write(&hook, script).expect("hook");
            let mut permissions = fs::metadata(&hook).expect("metadata").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&hook, permissions).expect("executable hook");
        }
        let editor = repository.root.join("editor-marker");
        repository.write("template.txt", "template must not appear\n");
        repository.git_ok(["config", "core.editor", &format!("{}", editor.display())]);
        repository.git_ok(["config", "commit.template", "template.txt"]);
        repository.git_ok(["config", "commit.cleanup", "strip"]);
        repository.git_ok(["config", "commit.verbose", "true"]);

        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(repository.root.clone())
            .expect("open fixture");
        let changes = registry
            .repository_changes(&opened.repository_id)
            .expect("changes");
        let receipt = registry
            .create_commit(
                &opened.repository_id,
                changes.change_set_id.as_str(),
                "subject\n\n body\n",
            )
            .expect("commit");
        assert_eq!(receipt.outcome, MutationOutcome::Applied);
        let message = repository.git_output(["log", "-1", "--format=%B"]);
        assert!(message.contains("prepare hook"));
        assert!(message.contains("commit hook"));
        assert!(!message.contains("template must not appear"));
        assert!(!message.contains("diff --git"));
        assert!(!editor.exists());
        assert!(marker.exists());
    }

    #[cfg(unix)]
    #[test]
    fn commit_supports_detached_head_and_rejects_commit_msg_hook() {
        let detached = TestRepository::new();
        detached.commit_file("base.txt", "base\n", "base");
        detached.git_ok(["checkout", "--detach", "HEAD"]);
        detached.write("detached.txt", "next\n");
        detached.git_ok(["add", "--", "detached.txt"]);
        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(detached.root.clone())
            .expect("open detached fixture");
        let changes = registry
            .repository_changes(&opened.repository_id)
            .expect("changes");
        let receipt = registry
            .create_commit(
                &opened.repository_id,
                changes.change_set_id.as_str(),
                "detached",
            )
            .expect("commit");
        assert_eq!(receipt.outcome, MutationOutcome::Applied);
        assert_eq!(
            detached.git_output(["rev-parse", "--abbrev-ref", "HEAD"]),
            "HEAD"
        );

        let rejected = TestRepository::new();
        rejected.commit_file("base.txt", "base\n", "base");
        rejected.write("next.txt", "next\n");
        rejected.git_ok(["add", "--", "next.txt"]);
        let hook = rejected.root.join(".git/hooks/commit-msg");
        fs::write(&hook, "#!/bin/sh\necho commit-msg-rejected >&2\nexit 1\n").expect("hook");
        let mut permissions = fs::metadata(&hook).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&hook, permissions).expect("executable hook");
        let opened = registry
            .open(rejected.root.clone())
            .expect("open rejected fixture");
        let changes = registry
            .repository_changes(&opened.repository_id)
            .expect("changes");
        let before = rejected.oid("HEAD");
        let receipt = registry
            .create_commit(
                &opened.repository_id,
                changes.change_set_id.as_str(),
                "rejected",
            )
            .expect("receipt");
        assert_eq!(receipt.outcome, MutationOutcome::Rejected);
        assert_eq!(rejected.oid("HEAD"), before);
    }

    #[test]
    fn commit_rejects_stale_change_set_before_starting() {
        let repository = TestRepository::new();
        repository.commit_file("base.txt", "base\n", "base");
        repository.write("next.txt", "next\n");
        repository.git_ok(["add", "--", "next.txt"]);
        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(repository.root.clone())
            .expect("open fixture");
        let stale = registry
            .repository_changes(&opened.repository_id)
            .expect("first changes");
        let fresh = registry
            .repository_changes(&opened.repository_id)
            .expect("replacement changes");
        let error = registry
            .create_commit(
                &opened.repository_id,
                stale.change_set_id.as_str(),
                "message",
            )
            .expect_err("stale changes");
        assert_eq!(error.code, "change_set_unavailable");
        registry
            .create_commit(
                &opened.repository_id,
                fresh.change_set_id.as_str(),
                "message",
            )
            .expect("fresh commit");
    }

    #[cfg(unix)]
    #[test]
    fn commit_preserves_configured_signing_failure() {
        let repository = TestRepository::new();
        repository.commit_file("base.txt", "base\n", "base");
        repository.write("next.txt", "next\n");
        repository.git_ok(["add", "--", "next.txt"]);
        let signer = repository.root.join("failing-signer");
        let marker = repository.root.join("signer-ran");
        fs::write(&signer, "#!/bin/sh\n: > signer-ran\nexit 1\n").expect("signer");
        let mut permissions = fs::metadata(&signer).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&signer, permissions).expect("executable signer");
        repository.git_ok(["config", "commit.gpgSign", "true"]);
        repository.git_ok([
            "config",
            "gpg.program",
            signer.to_str().expect("UTF-8 signer"),
        ]);
        let registry = RepositoryRegistry::default();
        let opened = registry
            .open(repository.root.clone())
            .expect("open fixture");
        let changes = registry
            .repository_changes(&opened.repository_id)
            .expect("changes");
        let receipt = registry
            .create_commit(
                &opened.repository_id,
                changes.change_set_id.as_str(),
                "signed",
            )
            .expect("receipt");
        assert_eq!(receipt.outcome, MutationOutcome::Rejected);
        assert!(
            marker.exists(),
            "configured signer must execute for explicit commit"
        );
    }

    #[cfg(unix)]
    fn commit_lifecycle_fixture(
        hook_name: &str,
        script: &str,
    ) -> (
        TestRepository,
        RepositoryRegistry,
        String,
        RepositoryChanges,
        String,
    ) {
        let repository = TestRepository::new();
        repository.commit_file("base.txt", "base\n", "Base");
        repository.commit_file("second.txt", "second\n", "Second");
        repository.write("next.txt", "next\n");
        repository.git_ok(["add", "--", "next.txt"]);
        let hook = repository.git_path(&format!("hooks/{hook_name}"));
        fs::write(&hook, script).expect("write lifecycle hook");
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).expect("executable hook");
        let runner = GitRunner::default().with_mutation_deadline(Duration::from_millis(500));
        #[cfg(target_os = "linux")]
        let runner = runner.with_mutation_pipe_capacity(4096);
        let registry = RepositoryRegistry::with_runner(runner);
        let opened = registry
            .open(repository.root.clone())
            .expect("open fixture");
        let cursor = registry
            .commit_history_page(&opened.repository_id, None, Some(1))
            .expect("history snapshot")
            .next_cursor
            .expect("old history cursor")
            .as_str()
            .to_owned();
        let changes = registry
            .repository_changes(&opened.repository_id)
            .expect("changes");
        (repository, registry, opened.repository_id, changes, cursor)
    }

    #[cfg(unix)]
    fn assert_commit_authority_retired(
        repository: &TestRepository,
        registry: &RepositoryRegistry,
        repository_id: &str,
        changes: &RepositoryChanges,
        cursor: &str,
    ) {
        assert_eq!(
            registry
                .changes
                .authorize_change_set(
                    repository_id,
                    &repository.root,
                    changes.change_set_id.as_str(),
                )
                .expect_err("old changes retired")
                .code,
            "change_set_unavailable",
        );
        assert_eq!(
            registry
                .commit_history_page(repository_id, Some(cursor), Some(1))
                .expect_err("old history retired even without proven HEAD movement")
                .code,
            "history_session_unavailable",
        );
        assert_eq!(
            fs::read_to_string(repository.git_path("orbit-hook-runs")).expect("hook count"),
            "run\n",
            "a started commit must never be retried automatically",
        );
    }

    #[cfg(unix)]
    #[test]
    fn commit_large_stdin_and_hook_output_share_the_mutation_deadline() {
        let (repository, registry, id, changes, cursor) = commit_lifecycle_fixture(
            "pre-commit",
            // Git runs pre-commit before consuming --file=-. Both pipes exceed
            // the explicit 4-KiB test pipes, reproducing the old write ordering.
            "#!/bin/sh\necho run >> .git/orbit-hook-runs\ntrap 'wait; exit 0' TERM\nsleep 30 &\necho $! > .git/orbit-descendant\ndd if=/dev/zero bs=1024 count=48 2>/dev/null\nwait\n",
        );
        let before = repository.oid("HEAD");
        let message = "x".repeat(MAX_COMMIT_MESSAGE_BYTES);
        let started = std::time::Instant::now();
        let receipt = registry
            .create_commit(&id, changes.change_set_id.as_str(), &message)
            .expect("started timeout returns receipt rather than a stdin error");
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "stdin cannot bypass the deadline"
        );
        assert_eq!(receipt.outcome, MutationOutcome::Uncertain);
        assert_eq!(
            receipt.issue.as_ref().map(|issue| issue.code),
            Some("mutation_timed_out")
        );
        assert!(!receipt.head_changed);
        assert!(receipt.commit_oid.is_none());
        assert_eq!(repository.oid("HEAD"), before);
        assert_eq!(
            receipt
                .repository_changes
                .as_ref()
                .expect("post status")
                .summary
                .staged,
            1
        );
        assert_commit_authority_retired(&repository, &registry, &id, &changes, &cursor);
        let pid =
            fs::read_to_string(repository.git_path("orbit-descendant")).expect("descendant PID");
        wait_for_process_exit(pid.trim());
    }

    #[cfg(unix)]
    fn wait_for_process_exit(pid: &str) {
        let path = Path::new("/proc").join(pid);
        for _ in 0..100 {
            if !path.exists() {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("ordinary commit descendant {pid} was not cleaned up");
    }

    #[cfg(unix)]
    #[test]
    fn commit_hook_output_overflow_cancels_large_stdin_without_retry() {
        let (repository, registry, id, changes, cursor) = commit_lifecycle_fixture(
            "pre-commit",
            "#!/bin/sh\necho run >> .git/orbit-hook-runs\ndd if=/dev/zero bs=1024 count=128 >&2 2>/dev/null\nsleep 30\n",
        );
        let before = repository.oid("HEAD");
        let started = std::time::Instant::now();
        let receipt = registry
            .create_commit(
                &id,
                changes.change_set_id.as_str(),
                &"x".repeat(MAX_COMMIT_MESSAGE_BYTES),
            )
            .expect("output cancellation receipt");
        assert!(started.elapsed() < Duration::from_secs(4));
        assert_eq!(receipt.outcome, MutationOutcome::Uncertain);
        let issue = receipt.issue.as_ref().expect("output diagnostic");
        assert_eq!(issue.code, "mutation_outcome_uncertain");
        assert!(issue.message.contains("more output"));
        assert!(issue
            .details
            .as_ref()
            .is_none_or(|detail| detail.chars().count() <= 512));
        assert!(!receipt.head_changed);
        assert_eq!(repository.oid("HEAD"), before);
        assert_commit_authority_retired(&repository, &registry, &id, &changes, &cursor);
    }

    #[cfg(unix)]
    #[test]
    fn commit_timeout_kills_term_resistant_hook_after_git_exits() {
        let (repository, registry, id, changes, cursor) = commit_lifecycle_fixture(
            "pre-commit",
            "#!/bin/sh\necho run >> .git/orbit-hook-runs\ntrap '' TERM\necho $$ > .git/orbit-hook-pid\nsleep 30 &\necho $! > .git/orbit-descendant\nwait\n",
        );
        let before = repository.oid("HEAD");
        let started = std::time::Instant::now();
        let receipt = registry
            .create_commit(
                &id,
                changes.change_set_id.as_str(),
                &"x".repeat(MAX_COMMIT_MESSAGE_BYTES),
            )
            .expect("resistant hook timeout receipt");
        assert!(started.elapsed() < Duration::from_secs(4));
        assert_eq!(receipt.outcome, MutationOutcome::Uncertain);
        assert_eq!(
            receipt.issue.as_ref().map(|issue| issue.code),
            Some("mutation_timed_out")
        );
        assert_eq!(repository.oid("HEAD"), before);
        for name in ["orbit-hook-pid", "orbit-descendant"] {
            let pid = fs::read_to_string(repository.git_path(name)).expect("process PID");
            wait_for_process_exit(pid.trim());
        }
        assert_commit_authority_retired(&repository, &registry, &id, &changes, &cursor);
    }

    #[cfg(unix)]
    #[test]
    fn commit_post_hook_timeout_reports_verified_head_with_warning_and_preserves_lock() {
        let (repository, registry, id, changes, cursor) = commit_lifecycle_fixture(
            "post-commit",
            "#!/bin/sh\necho run >> .git/orbit-hook-runs\necho owned-by-fixture > .git/index.lock\ntrap 'wait; exit 0' TERM\nsleep 30 &\necho $! > .git/orbit-descendant\nwait\n",
        );
        let before = repository.oid("HEAD");
        let receipt = registry
            .create_commit(
                &id,
                changes.change_set_id.as_str(),
                "Applied before timeout",
            )
            .expect("late timeout receipt");
        let after = repository.oid("HEAD");
        assert_ne!(before, after);
        assert_eq!(receipt.outcome, MutationOutcome::Applied);
        assert!(receipt.head_changed);
        assert_eq!(receipt.commit_oid.as_deref(), Some(after.as_str()));
        let issue = receipt
            .issue
            .as_ref()
            .expect("silent late timeout must warn");
        assert_eq!(issue.code, "mutation_warning");
        assert!(issue.message.contains("deadline"));
        assert!(issue
            .details
            .as_ref()
            .expect("lock detail")
            .contains("did not remove"));
        assert_eq!(
            fs::read_to_string(repository.git_path("index.lock")).expect("lock remains"),
            "owned-by-fixture\n"
        );
        assert_commit_authority_retired(&repository, &registry, &id, &changes, &cursor);
        let pid =
            fs::read_to_string(repository.git_path("orbit-descendant")).expect("descendant PID");
        wait_for_process_exit(pid.trim());
    }

    #[cfg(unix)]
    #[test]
    fn commit_unavailable_post_state_retires_history_without_claiming_head_changed() {
        let (repository, registry, id, changes, cursor) = commit_lifecycle_fixture(
            "post-commit",
            // A legal filter name containing '=' makes protected status fail
            // closed. This exercises real Git mutation and real refresh failure.
            "#!/bin/sh\necho run >> .git/orbit-hook-runs\ngit config 'filter.orbit=unavailable.clean' cat\n",
        );
        let before = repository.oid("HEAD");
        let receipt = registry
            .create_commit(
                &id,
                changes.change_set_id.as_str(),
                "Post-state unavailable",
            )
            .expect("uncertain receipt");
        assert_ne!(
            repository.oid("HEAD"),
            before,
            "commit really advanced HEAD"
        );
        assert_eq!(receipt.outcome, MutationOutcome::Uncertain);
        assert!(receipt.refresh_required);
        assert!(receipt.repository_changes.is_none());
        assert!(
            !receipt.head_changed,
            "unobserved HEAD movement must not be claimed"
        );
        assert!(receipt.commit_oid.is_none());
        assert_commit_authority_retired(&repository, &registry, &id, &changes, &cursor);
    }

    #[cfg(unix)]
    #[test]
    fn commit_large_message_reaches_eof_after_hook_output_is_drained() {
        let (repository, registry, id, changes, cursor) = commit_lifecycle_fixture(
            "pre-commit",
            "#!/bin/sh\necho run >> .git/orbit-hook-runs\ndd if=/dev/zero bs=1024 count=48 2>/dev/null\n",
        );
        let message = format!("{}\n", "x".repeat(MAX_COMMIT_MESSAGE_BYTES - 1));
        let receipt = registry
            .create_commit(&id, changes.change_set_id.as_str(), &message)
            .expect("commit must receive EOF");
        assert_eq!(receipt.outcome, MutationOutcome::Applied);
        assert_eq!(
            repository.git_bytes(["log", "-1", "--format=%B"]),
            format!("{message}\n").as_bytes()
        );
        assert_commit_authority_retired(&repository, &registry, &id, &changes, &cursor);
    }

    #[cfg(unix)]
    #[test]
    fn commit_external_index_race_returns_actual_post_state_and_retires_authority() {
        let (repository, registry, id, changes, cursor) = commit_lifecycle_fixture(
            "pre-commit",
            "#!/bin/sh\necho run >> .git/orbit-hook-runs\n: > .git/orbit-hook-entered\nwhile [ ! -e .git/orbit-hook-release ]; do sleep 0.01; done\n",
        );
        let before = repository.oid("HEAD");
        thread::scope(|scope| {
            let worker = scope.spawn(|| {
                registry.create_commit(&id, changes.change_set_id.as_str(), "Raced index")
            });
            wait_for_path(&repository.git_path("orbit-hook-entered"));
            repository.git_ok(["reset", "--mixed", "--no-refresh", "HEAD"]);
            fs::write(repository.git_path("orbit-hook-release"), b"release").expect("release hook");
            let receipt = worker.join().expect("commit worker").expect("race receipt");
            assert_eq!(receipt.outcome, MutationOutcome::Applied);
            assert!(receipt.head_changed);
            assert_eq!(
                receipt.commit_oid.as_deref(),
                Some(repository.oid("HEAD").as_str())
            );
            let actual = read_detailed_status(&GitRunner::default(), &repository.root)
                .expect("actual post state");
            assert_eq!(
                receipt
                    .repository_changes
                    .as_ref()
                    .expect("post state")
                    .summary,
                actual.working_tree
            );
        });
        assert_ne!(repository.oid("HEAD"), before);
        assert_eq!(repository.git_output(["rev-list", "--count", "HEAD"]), "3");
        assert_commit_authority_retired(&repository, &registry, &id, &changes, &cursor);
    }
}
