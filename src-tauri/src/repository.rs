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
    change_sets::{AuthorizedFile, ChangeSetRegistry, RepositoryChanges},
    error::OrbitError,
    git::{
        read_detailed_status, read_file_diff, read_recent_commits, read_status, CommitSummary,
        DiffSelection, DiffSide, FileDiff, GitRunner, HeadSnapshot, WorkingTreeSnapshot,
    },
    history_sessions::{CommitHistoryPage, CommitHistoryRegistry},
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
    histories: CommitHistoryRegistry,
    changes: ChangeSetRegistry,
}

impl Default for RepositoryRegistry {
    fn default() -> Self {
        Self {
            roots: RwLock::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            runner: GitRunner::default(),
            histories: CommitHistoryRegistry::default(),
            changes: ChangeSetRegistry::default(),
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
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

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

        fn oid(&self, revision: &str) -> String {
            self.git_output(["rev-parse", "--verify", revision])
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
}
