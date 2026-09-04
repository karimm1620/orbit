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
    error::OrbitError,
    git::{
        read_recent_commits, read_status, CommitSummary, GitRunner, HeadSnapshot,
        WorkingTreeSnapshot,
    },
};

const REPOSITORY_PROBE_LIMIT: usize = 16 * 1024;

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
}

impl Default for RepositoryRegistry {
    fn default() -> Self {
        Self {
            roots: RwLock::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            runner: GitRunner::default(),
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
        if !valid_repository_id(repository_id) {
            return Err(OrbitError::repository_unavailable(
                "This repository is no longer authorized in the current Orbit session.",
            ));
        }

        let root = self
            .roots
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
            })?;
        let resolved = resolve_repository(&self.runner, &root).map_err(|error| {
            if error.code == "not_a_repository" {
                OrbitError::repository_unavailable(
                    "The opened directory is no longer an available Git working tree.",
                )
            } else {
                error
            }
        })?;

        if resolved != root {
            return Err(OrbitError::repository_unavailable(
                "The opened repository now resolves to a different working tree.",
            ));
        }

        build_snapshot(&self.runner, repository_id.to_owned(), &root)
    }

    fn next_repository_id(&self) -> String {
        let value = self.next_id.fetch_add(1, Ordering::Relaxed);
        format!("repository-{value:016x}")
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
        return Err(OrbitError::not_a_repository());
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
    let root = std::str::from_utf8(&root.stdout).map_err(|_| {
        OrbitError::unsupported(
            "resolve_repository_root",
            "The repository root path is not valid UTF-8.",
        )
    })?;
    let root = PathBuf::from(root.trim_end_matches(['\n', '\r']));

    fs::canonicalize(root).map_err(|_| {
        OrbitError::repository_unavailable("The Git working-tree root cannot be read.")
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
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

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

        fn commit_file(&self, path: &str, contents: &str, message: &str) {
            self.write(path, contents);
            self.git_ok(["add", "--", path]);
            self.git_ok(["commit", "-m", message]);
        }

        fn git_ok<const N: usize>(&self, args: [&str; N]) {
            let output = GitRunner::default()
                .run(Some(&self.root), "test_fixture", args, 1024 * 1024)
                .expect("run fixture git command");
            assert!(
                output.status.success(),
                "fixture git command failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
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
}
