use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};

use serde::Serialize;

use crate::{
    error::OrbitError,
    git::{
        ChangeFacet, ConflictKind, HeadSnapshot, StatusEntry, SubmoduleState, WorkingTreeSnapshot,
    },
};

pub const MAX_ACTIVE_CHANGE_SETS: usize = 8;
const CHANGE_SET_IDLE_TTL: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ChangeSetId(String);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct FileId(String);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ChangesRefreshGeneration(u64);

impl ChangeSetId {
    #[cfg(test)]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FileId {
    #[cfg(test)]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryPathDisplay {
    pub text: String,
    pub escaped: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangedFile {
    pub file_id: FileId,
    pub path: RepositoryPathDisplay,
    pub original_path: Option<RepositoryPathDisplay>,
    pub staged: Option<ChangeFacet>,
    pub unstaged: Option<ChangeFacet>,
    pub conflict: Option<ConflictKind>,
    pub submodule: Option<SubmoduleState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryChanges {
    pub repository_id: String,
    pub change_set_id: ChangeSetId,
    pub head: HeadSnapshot,
    pub summary: WorkingTreeSnapshot,
    pub files: Vec<ChangedFile>,
}

#[derive(Debug, Clone)]
struct StoredFile {
    path: Vec<u8>,
    original_path: Option<Vec<u8>>,
    staged: Option<ChangeFacet>,
    unstaged: Option<ChangeFacet>,
    conflict: Option<ConflictKind>,
    submodule: Option<SubmoduleState>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuthorizedFile {
    pub path: Vec<u8>,
    pub original_path: Option<Vec<u8>>,
    pub staged: Option<ChangeFacet>,
    pub unstaged: Option<ChangeFacet>,
    pub conflict: Option<ConflictKind>,
    pub submodule: Option<SubmoduleState>,
}

impl From<&StoredFile> for AuthorizedFile {
    fn from(file: &StoredFile) -> Self {
        Self {
            path: file.path.clone(),
            original_path: file.original_path.clone(),
            staged: file.staged.clone(),
            unstaged: file.unstaged.clone(),
            conflict: file.conflict,
            submodule: file.submodule,
        }
    }
}

impl From<StatusEntry> for StoredFile {
    fn from(entry: StatusEntry) -> Self {
        Self {
            path: entry.path,
            original_path: entry.original_path,
            staged: entry.staged,
            unstaged: entry.unstaged,
            conflict: entry.conflict,
            submodule: entry.submodule,
        }
    }
}

impl StoredFile {
    fn to_model(&self, file_id: &str) -> ChangedFile {
        ChangedFile {
            file_id: FileId(file_id.to_owned()),
            path: display_path(&self.path),
            original_path: self.original_path.as_deref().map(display_path),
            staged: self.staged.clone(),
            unstaged: self.unstaged.clone(),
            conflict: self.conflict,
            submodule: self.submodule,
        }
    }
}

#[derive(Debug)]
struct ChangeSet {
    repository_id: String,
    root: PathBuf,
    files: HashMap<String, StoredFile>,
    last_access: Instant,
}

#[derive(Debug)]
struct RefreshAuthority {
    generation: ChangesRefreshGeneration,
    root: PathBuf,
}

#[derive(Default)]
struct ChangeSetRegistryState {
    change_sets: HashMap<String, ChangeSet>,
    latest_refreshes: HashMap<String, RefreshAuthority>,
}

pub struct ChangeSetRegistry {
    state: Mutex<ChangeSetRegistryState>,
    next_refresh: AtomicU64,
    next_change_set: AtomicU64,
    next_file: AtomicU64,
}

impl Default for ChangeSetRegistry {
    fn default() -> Self {
        Self {
            state: Mutex::new(ChangeSetRegistryState::default()),
            next_refresh: AtomicU64::new(1),
            next_change_set: AtomicU64::new(1),
            next_file: AtomicU64::new(1),
        }
    }
}

impl ChangeSetRegistry {
    pub fn reserve_refresh(&self) -> ChangesRefreshGeneration {
        ChangesRefreshGeneration(self.next_refresh.fetch_add(1, Ordering::Relaxed))
    }

    pub fn begin_refresh(
        &self,
        repository_id: &str,
        root: &Path,
        generation: ChangesRefreshGeneration,
    ) -> Result<ChangesRefreshGeneration, OrbitError> {
        let mut state = self.lock_state()?;
        if state
            .latest_refreshes
            .get(repository_id)
            .is_some_and(|latest| latest.generation.0 >= generation.0)
        {
            return Err(OrbitError::changes_refresh_superseded());
        }
        state.latest_refreshes.insert(
            repository_id.to_owned(),
            RefreshAuthority {
                generation,
                root: root.to_owned(),
            },
        );
        Ok(generation)
    }

    pub fn install(
        &self,
        repository_id: &str,
        root: &Path,
        generation: ChangesRefreshGeneration,
        head: HeadSnapshot,
        summary: WorkingTreeSnapshot,
        entries: Vec<StatusEntry>,
    ) -> Result<RepositoryChanges, OrbitError> {
        let change_set_id = self.new_change_set_id();
        let file_ids = (0..entries.len())
            .map(|_| self.new_file_id())
            .collect::<Vec<_>>();
        let files = file_ids
            .iter()
            .cloned()
            .zip(entries.into_iter().map(StoredFile::from))
            .collect::<HashMap<_, _>>();
        let now = Instant::now();
        let mut state = self.lock_state()?;
        let is_current = state
            .latest_refreshes
            .get(repository_id)
            .is_some_and(|latest| latest.generation == generation && latest.root == root);
        if !is_current {
            return Err(OrbitError::changes_refresh_superseded());
        }
        prune_expired(&mut state.change_sets, now);

        // Only a fully parsed status result reaches installation. Replacing the old
        // repository entry while checking its generation inside this same lock makes
        // request start order authoritative rather than worker completion order.
        state
            .change_sets
            .retain(|_, change_set| change_set.repository_id != repository_id);
        if state.change_sets.len() >= MAX_ACTIVE_CHANGE_SETS {
            let oldest = state
                .change_sets
                .iter()
                .min_by(|(left_id, left), (right_id, right)| {
                    left.last_access
                        .cmp(&right.last_access)
                        .then_with(|| left_id.cmp(right_id))
                })
                .map(|(id, _)| id.clone())
                .ok_or_else(OrbitError::change_set_unavailable)?;
            state.change_sets.remove(&oldest);
        }
        state.change_sets.insert(
            change_set_id.clone(),
            ChangeSet {
                repository_id: repository_id.to_owned(),
                root: root.to_owned(),
                files,
                last_access: now,
            },
        );

        let models = file_ids
            .iter()
            .map(|file_id| {
                authorize_file(
                    &mut state.change_sets,
                    repository_id,
                    root,
                    &change_set_id,
                    file_id,
                    now,
                )
                .map(|file| file.to_model(file_id))
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(RepositoryChanges {
            repository_id: repository_id.to_owned(),
            change_set_id: ChangeSetId(change_set_id),
            head,
            summary,
            files: models,
        })
    }

    pub fn invalidate_repository(&self, repository_id: &str) -> Result<(), OrbitError> {
        let mut state = self.lock_state()?;
        state
            .change_sets
            .retain(|_, change_set| change_set.repository_id != repository_id);
        Ok(())
    }

    pub(crate) fn authorize_file(
        &self,
        repository_id: &str,
        root: &Path,
        change_set_id: &str,
        file_id: &str,
    ) -> Result<AuthorizedFile, OrbitError> {
        let mut state = self.lock_state()?;
        authorize_file(
            &mut state.change_sets,
            repository_id,
            root,
            change_set_id,
            file_id,
            Instant::now(),
        )
        .map(AuthorizedFile::from)
    }

    #[cfg(test)]
    pub fn authorize_for_test(
        &self,
        repository_id: &str,
        root: &Path,
        change_set_id: &str,
        file_id: &str,
    ) -> Result<(), OrbitError> {
        let mut state = self.lock_state()?;
        authorize_file(
            &mut state.change_sets,
            repository_id,
            root,
            change_set_id,
            file_id,
            Instant::now(),
        )
        .map(|_| ())
    }

    fn lock_state(&self) -> Result<std::sync::MutexGuard<'_, ChangeSetRegistryState>, OrbitError> {
        self.state.lock().map_err(|_| {
            OrbitError::internal(
                "read_repository_changes",
                "Repository change-set state is unavailable.",
            )
        })
    }

    fn new_change_set_id(&self) -> String {
        let value = self.next_change_set.fetch_add(1, Ordering::Relaxed);
        format!("change-set-{value:016x}")
    }

    fn new_file_id(&self) -> String {
        let value = self.next_file.fetch_add(1, Ordering::Relaxed);
        format!("file-{value:016x}")
    }
}

fn authorize_file<'a>(
    change_sets: &'a mut HashMap<String, ChangeSet>,
    repository_id: &str,
    root: &Path,
    change_set_id: &str,
    file_id: &str,
    now: Instant,
) -> Result<&'a StoredFile, OrbitError> {
    if !valid_id(change_set_id, "change-set-") || !valid_id(file_id, "file-") {
        return Err(OrbitError::change_set_unavailable());
    }
    prune_expired(change_sets, now);
    let change_set = change_sets
        .get_mut(change_set_id)
        .filter(|change_set| change_set.repository_id == repository_id && change_set.root == root)
        .ok_or_else(OrbitError::change_set_unavailable)?;
    let file = change_set
        .files
        .get(file_id)
        .ok_or_else(OrbitError::change_set_unavailable)?;
    change_set.last_access = now;
    Ok(file)
}

fn prune_expired(change_sets: &mut HashMap<String, ChangeSet>, now: Instant) {
    change_sets.retain(|_, change_set| {
        now.saturating_duration_since(change_set.last_access) <= CHANGE_SET_IDLE_TTL
    });
}

fn valid_id(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|suffix| {
        suffix.len() == 16 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

fn display_path(path: &[u8]) -> RepositoryPathDisplay {
    let mut text = String::new();
    let mut escaped = false;
    let mut remaining = path;

    while !remaining.is_empty() {
        match std::str::from_utf8(remaining) {
            Ok(valid) => {
                append_display_text(valid, &mut text, &mut escaped);
                break;
            }
            Err(error) => {
                let valid_up_to = error.valid_up_to();
                let valid = std::str::from_utf8(&remaining[..valid_up_to])
                    .expect("UTF-8 validator identified a valid prefix");
                append_display_text(valid, &mut text, &mut escaped);
                let invalid_length = error.error_len().unwrap_or(remaining.len() - valid_up_to);
                for byte in &remaining[valid_up_to..valid_up_to + invalid_length] {
                    text.push_str(&format!("\\x{byte:02X}"));
                }
                escaped = true;
                remaining = &remaining[valid_up_to + invalid_length..];
            }
        }
    }

    RepositoryPathDisplay { text, escaped }
}

fn append_display_text(value: &str, output: &mut String, escaped: &mut bool) {
    for character in value.chars() {
        match character {
            '\\' => {
                output.push_str("\\\\");
                *escaped = true;
            }
            '\n' => {
                output.push_str("\\n");
                *escaped = true;
            }
            '\r' => {
                output.push_str("\\r");
                *escaped = true;
            }
            '\t' => {
                output.push_str("\\t");
                *escaped = true;
            }
            character if character.is_control() => {
                for byte in character.to_string().as_bytes() {
                    output.push_str(&format!("\\x{byte:02X}"));
                }
                *escaped = true;
            }
            character => output.push(character),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &[u8]) -> StatusEntry {
        StatusEntry {
            path: path.to_vec(),
            original_path: None,
            staged: None,
            unstaged: None,
            conflict: None,
            submodule: None,
        }
    }

    fn head() -> HeadSnapshot {
        HeadSnapshot {
            oid: None,
            branch: Some("main".into()),
            detached: false,
            upstream: None,
            ahead: None,
            behind: None,
        }
    }

    fn summary() -> WorkingTreeSnapshot {
        WorkingTreeSnapshot {
            staged: 0,
            unstaged: 0,
            untracked: 1,
            conflicted: 0,
            clean: false,
        }
    }

    fn install_changes(
        registry: &ChangeSetRegistry,
        repository_id: &str,
        root: &Path,
        entries: Vec<StatusEntry>,
    ) -> RepositoryChanges {
        let generation = registry
            .begin_refresh(repository_id, root, registry.reserve_refresh())
            .expect("begin change refresh");
        registry
            .install(repository_id, root, generation, head(), summary(), entries)
            .expect("install change set")
    }

    #[test]
    fn escapes_controls_backslashes_and_invalid_bytes_without_losing_unicode() {
        let display = display_path("unicodé\\tab\tline\n".as_bytes());
        assert_eq!(display.text, "unicodé\\\\tab\\tline\\n");
        assert!(display.escaped);

        let display = display_path(b"invalid-\xff.txt");
        assert_eq!(display.text, "invalid-\\xFF.txt");
        assert!(display.escaped);

        let display = display_path("plain/文件.txt".as_bytes());
        assert_eq!(display.text, "plain/文件.txt");
        assert!(!display.escaped);
    }

    #[test]
    fn replacement_invalidates_old_handles_and_rejects_cross_repository_access() {
        let registry = ChangeSetRegistry::default();
        let root = Path::new("/tmp/orbit-change-set-a");
        let first = install_changes(
            &registry,
            "repository-0000000000000001",
            root,
            vec![entry(b"a")],
        );
        let second = install_changes(
            &registry,
            "repository-0000000000000001",
            root,
            vec![entry(b"b")],
        );
        let first_file = first.files[0].file_id.0.clone();
        let second_file = second.files[0].file_id.0.clone();
        let mut state = registry.lock_state().expect("lock change sets");

        assert!(authorize_file(
            &mut state.change_sets,
            "repository-0000000000000001",
            root,
            &first.change_set_id.0,
            &first_file,
            Instant::now(),
        )
        .is_err());
        assert!(authorize_file(
            &mut state.change_sets,
            "repository-0000000000000002",
            root,
            &second.change_set_id.0,
            &second_file,
            Instant::now(),
        )
        .is_err());
        assert!(authorize_file(
            &mut state.change_sets,
            "repository-0000000000000001",
            Path::new("/tmp/orbit-change-set-b"),
            &second.change_set_id.0,
            &second_file,
            Instant::now(),
        )
        .is_err());
        assert!(authorize_file(
            &mut state.change_sets,
            "repository-0000000000000001",
            root,
            &second.change_set_id.0,
            "file-000000000000ffff",
            Instant::now(),
        )
        .is_err());
    }

    #[test]
    fn older_completion_cannot_replace_a_newer_successful_refresh() {
        let registry = ChangeSetRegistry::default();
        let repository_id = "repository-0000000000000001";
        let root = Path::new("/tmp/orbit-refresh-order");
        let prior = install_changes(&registry, repository_id, root, vec![entry(b"prior")]);

        let older = registry.reserve_refresh();
        registry
            .begin_refresh(repository_id, root, older)
            .expect("begin older refresh");
        let newer = registry.reserve_refresh();
        registry
            .begin_refresh(repository_id, root, newer)
            .expect("begin newer refresh");
        let newest = registry
            .install(
                repository_id,
                root,
                newer,
                head(),
                summary(),
                vec![entry(b"newest")],
            )
            .expect("newer refresh succeeds first");

        let superseded = registry
            .install(
                repository_id,
                root,
                older,
                head(),
                summary(),
                vec![entry(b"older")],
            )
            .expect_err("older completion cannot install afterward");
        assert_eq!(superseded.code, "changes_refresh_superseded");
        registry
            .authorize_for_test(
                repository_id,
                root,
                newest.change_set_id.as_str(),
                newest.files[0].file_id.as_str(),
            )
            .expect("newer change set remains authoritative");
        assert!(registry
            .authorize_for_test(
                repository_id,
                root,
                prior.change_set_id.as_str(),
                prior.files[0].file_id.as_str(),
            )
            .is_err());
    }

    #[test]
    fn newer_failed_refresh_preserves_prior_set_and_still_supersedes_older_work() {
        let registry = ChangeSetRegistry::default();
        let repository_id = "repository-0000000000000001";
        let root = Path::new("/tmp/orbit-failed-refresh-order");
        let prior = install_changes(&registry, repository_id, root, vec![entry(b"prior")]);

        let older = registry.reserve_refresh();
        registry
            .begin_refresh(repository_id, root, older)
            .expect("begin older refresh");
        let newer = registry.reserve_refresh();
        registry
            .begin_refresh(repository_id, root, newer)
            .expect("begin newer refresh that later fails");
        // A status/capability failure never calls install for the newer request.
        registry
            .authorize_for_test(
                repository_id,
                root,
                prior.change_set_id.as_str(),
                prior.files[0].file_id.as_str(),
            )
            .expect("newer failure preserves the prior successful set");

        let superseded = registry
            .install(
                repository_id,
                root,
                older,
                head(),
                summary(),
                vec![entry(b"older")],
            )
            .expect_err("older completion stays superseded after newer failure");
        assert_eq!(superseded.code, "changes_refresh_superseded");
        registry
            .authorize_for_test(
                repository_id,
                root,
                prior.change_set_id.as_str(),
                prior.files[0].file_id.as_str(),
            )
            .expect("prior set remains authoritative");
    }

    #[test]
    fn delayed_authorization_cannot_reverse_reserved_request_order() {
        let registry = ChangeSetRegistry::default();
        let repository_id = "repository-0000000000000001";
        let root = Path::new("/tmp/orbit-delayed-authorization");
        let older = registry.reserve_refresh();
        let newer = registry.reserve_refresh();

        registry
            .begin_refresh(repository_id, root, newer)
            .expect("newer worker completes authorization first");
        let superseded = registry
            .begin_refresh(repository_id, root, older)
            .expect_err("older authorization cannot take authority later");
        assert_eq!(superseded.code, "changes_refresh_superseded");
    }

    #[test]
    fn repository_invalidation_does_not_revive_an_older_in_flight_refresh() {
        let registry = ChangeSetRegistry::default();
        let repository_id = "repository-0000000000000001";
        let root = Path::new("/tmp/orbit-invalidated-refresh");
        let older = registry.reserve_refresh();
        registry
            .begin_refresh(repository_id, root, older)
            .expect("begin older refresh");
        let newer = registry.reserve_refresh();
        registry
            .begin_refresh(repository_id, root, newer)
            .expect("begin newer refresh");

        registry
            .invalidate_repository(repository_id)
            .expect("invalidate repository handles");
        let superseded = registry
            .install(
                repository_id,
                root,
                older,
                head(),
                summary(),
                vec![entry(b"older")],
            )
            .expect_err("invalidation must retain the newer generation floor");
        assert_eq!(superseded.code, "changes_refresh_superseded");
    }

    #[test]
    fn expired_and_malformed_handles_are_unavailable() {
        let registry = ChangeSetRegistry::default();
        let root = Path::new("/tmp/orbit-change-set");
        let changes = install_changes(
            &registry,
            "repository-0000000000000001",
            root,
            vec![entry(b"a")],
        );
        let mut state = registry.lock_state().expect("lock change sets");
        let expired_at = state
            .change_sets
            .get(&changes.change_set_id.0)
            .expect("stored change set")
            .last_access
            + CHANGE_SET_IDLE_TTL
            + Duration::from_secs(1);

        let error = authorize_file(
            &mut state.change_sets,
            "repository-0000000000000001",
            root,
            &changes.change_set_id.0,
            &changes.files[0].file_id.0,
            expired_at,
        )
        .expect_err("expired handle should fail");
        assert_eq!(error.code, "change_set_unavailable");
        assert!(authorize_file(
            &mut state.change_sets,
            "repository-0000000000000001",
            root,
            "bad",
            "also-bad",
            Instant::now(),
        )
        .is_err());
    }

    #[test]
    fn registry_evicts_the_oldest_change_set_at_its_bound() {
        let registry = ChangeSetRegistry::default();
        let root = Path::new("/tmp/orbit-change-set");
        let mut first = None;
        for index in 0..=MAX_ACTIVE_CHANGE_SETS {
            let repository_id = format!("repository-{index:016x}");
            let changes = install_changes(&registry, &repository_id, root, vec![entry(b"a")]);
            if index == 0 {
                first = Some(changes.change_set_id.0);
            }
        }
        let state = registry.lock_state().expect("lock change sets");
        assert_eq!(state.change_sets.len(), MAX_ACTIVE_CHANGE_SETS);
        assert!(!state
            .change_sets
            .contains_key(&first.expect("first change set")));
    }
}
