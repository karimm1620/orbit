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
}

impl Default for RepositoryRegistry {
    fn default() -> Self {
        Self {
            roots: RwLock::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            runner: GitRunner::default(),
            histories: CommitHistoryRegistry::default(),
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

    fn authorized_root(&self, repository_id: &str) -> Result<PathBuf, OrbitError> {
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
        let resolved = match resolve_repository(&self.runner, &root) {
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
                }
                return Err(error);
            }
        };

        if resolved != root {
            self.histories.invalidate_repository(repository_id)?;
            return Err(OrbitError::repository_unavailable(
                "The opened repository now resolves to a different working tree.",
            ));
        }

        Ok(root)
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
        history_starting_tips, read_graph_commits, read_object_format, CommitHistoryHead,
        CommitRefKind,
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

        let error = read_graph_commits(
            &GitRunner::default(),
            &repository.root,
            &[missing_oid],
            1,
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
        let expected_commits = read_graph_commits(
            &registry.runner,
            &repository.root,
            &tips,
            200,
            object_format,
        )
        .expect("one-shot topological history");
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
    }
}
