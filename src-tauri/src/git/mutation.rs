use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;

use crate::error::OrbitError;

use super::{GitMutationOutput, GitRunner, StatusSnapshot};

const MUTATION_STDOUT_LIMIT: usize = 1024 * 1024;
const MUTATION_PROBE_LIMIT: usize = 16 * 1024;
const STAGING_DEADLINE: Duration = Duration::from_secs(120);
const COMMIT_DEADLINE: Duration = Duration::from_secs(300);
const TERMINATION_GRACE: Duration = Duration::from_secs(2);

const PSEUDOREFS: [&str; 4] = [
    "MERGE_HEAD",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "REBASE_HEAD",
];
const STATE_PATHS: [&str; 4] = ["rebase-merge", "rebase-apply", "sequencer", "SQUASH_MSG"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StagingOperation {
    StageFile,
    UnstageFile,
    StageAll,
    UnstageAll,
}

pub(crate) fn run_commit(
    runner: &GitRunner,
    root: &Path,
    message: &[u8],
) -> Result<GitMutationOutput, OrbitError> {
    let args = [
        "-c",
        "core.fsmonitor=false",
        "--no-pager",
        "--no-lazy-fetch",
        "--no-optional-locks",
        "commit",
        "--quiet",
        "--no-status",
        "--no-verbose",
        "--cleanup=verbatim",
        "--file=-",
    ]
    .map(OsString::from);
    runner.run_mutation_with_stdin(
        root,
        "create_commit",
        args,
        MUTATION_STDOUT_LIMIT,
        COMMIT_DEADLINE,
        TERMINATION_GRACE,
        Some(message),
    )
}

impl StagingOperation {
    pub fn name(self) -> &'static str {
        match self {
            Self::StageFile => "stage_file",
            Self::UnstageFile => "unstage_file",
            Self::StageAll => "stage_all",
            Self::UnstageAll => "unstage_all",
        }
    }
}

pub(crate) fn ensure_mutation_state_allowed(
    runner: &GitRunner,
    root: &Path,
    status: &StatusSnapshot,
) -> Result<(), OrbitError> {
    if status.changes.iter().any(|entry| entry.conflict.is_some()) {
        return Err(OrbitError::mutation_not_applicable(
            "Resolve the repository conflicts before staging or unstaging with Orbit.",
        ));
    }

    for reference in PSEUDOREFS {
        if pseudoref_exists(runner, root, reference)? {
            return Err(operation_in_progress());
        }
    }
    for name in STATE_PATHS {
        if git_state_path_exists(runner, root, name)? {
            return Err(operation_in_progress());
        }
    }
    Ok(())
}

pub(crate) fn run_stage_file(
    runner: &GitRunner,
    root: &Path,
    paths: &[Vec<u8>],
) -> Result<GitMutationOutput, OrbitError> {
    let mut args = stage_arguments(true);
    args.push(OsString::from("--"));
    args.extend(
        paths
            .iter()
            .map(|path| raw_argument(path))
            .collect::<Result<Vec<_>, _>>()?,
    );
    run(runner, root, StagingOperation::StageFile, args)
}

pub(crate) fn run_stage_all(
    runner: &GitRunner,
    root: &Path,
) -> Result<GitMutationOutput, OrbitError> {
    run(
        runner,
        root,
        StagingOperation::StageAll,
        stage_arguments(false),
    )
}

pub(crate) fn run_unstage_file(
    runner: &GitRunner,
    root: &Path,
    paths: &[Vec<u8>],
    unborn: bool,
) -> Result<GitMutationOutput, OrbitError> {
    let mut args = unstage_prefix(true);
    if unborn {
        args.extend(["rm", "--cached", "--force", "--quiet"].map(OsString::from));
    } else {
        args.extend(
            [
                "restore",
                "--staged",
                "--source=HEAD",
                "--no-recurse-submodules",
            ]
            .map(OsString::from),
        );
    }
    args.push(OsString::from("--"));
    args.extend(
        paths
            .iter()
            .map(|path| raw_argument(path))
            .collect::<Result<Vec<_>, _>>()?,
    );
    run(runner, root, StagingOperation::UnstageFile, args)
}

pub(crate) fn run_unstage_all(
    runner: &GitRunner,
    root: &Path,
    unborn: bool,
) -> Result<GitMutationOutput, OrbitError> {
    let mut args = unstage_prefix(false);
    if unborn {
        args.extend(["read-tree", "--empty"].map(OsString::from));
    } else {
        args.extend(
            [
                "reset",
                "--mixed",
                "--no-refresh",
                "--quiet",
                "--no-recurse-submodules",
                "HEAD",
            ]
            .map(OsString::from),
        );
    }
    run(runner, root, StagingOperation::UnstageAll, args)
}

pub(crate) fn index_lock_exists(runner: &GitRunner, root: &Path) -> bool {
    resolve_git_path(runner, root, "index.lock")
        .ok()
        .is_some_and(|path| fs::symlink_metadata(path).is_ok())
}

fn run(
    runner: &GitRunner,
    root: &Path,
    operation: StagingOperation,
    args: Vec<OsString>,
) -> Result<GitMutationOutput, OrbitError> {
    runner.run_mutation(
        root,
        operation.name(),
        args,
        MUTATION_STDOUT_LIMIT,
        STAGING_DEADLINE,
        TERMINATION_GRACE,
    )
}

fn stage_arguments(literal_paths: bool) -> Vec<OsString> {
    let mut args = common_prefix();
    args.extend(["-c", "add.ignoreErrors=false"].map(OsString::from));
    if literal_paths {
        args.push(OsString::from("--literal-pathspecs"));
    }
    args.extend(["add", "--all"].map(OsString::from));
    args
}

fn unstage_prefix(literal_paths: bool) -> Vec<OsString> {
    let mut args = common_prefix();
    args.extend(["-c", "submodule.recurse=false"].map(OsString::from));
    if literal_paths {
        args.push(OsString::from("--literal-pathspecs"));
    }
    args
}

fn common_prefix() -> Vec<OsString> {
    [
        "-c",
        "core.fsmonitor=false",
        "--no-pager",
        "--no-lazy-fetch",
        "--no-optional-locks",
    ]
    .map(OsString::from)
    .into()
}

fn pseudoref_exists(
    runner: &GitRunner,
    root: &Path,
    reference: &'static str,
) -> Result<bool, OrbitError> {
    let output = runner.run(
        Some(root),
        "check_mutation_state",
        [
            "--no-pager",
            "--no-lazy-fetch",
            "--no-optional-locks",
            "rev-parse",
            "--verify",
            "--quiet",
            reference,
        ],
        MUTATION_PROBE_LIMIT,
    )?;
    if output.status.success() {
        Ok(true)
    } else if output.status.code() == Some(1) {
        Ok(false)
    } else {
        Err(OrbitError::git_failed(
            "check_mutation_state",
            &output.stderr,
        ))
    }
}

fn git_state_path_exists(
    runner: &GitRunner,
    root: &Path,
    name: &'static str,
) -> Result<bool, OrbitError> {
    Ok(fs::symlink_metadata(resolve_git_path(runner, root, name)?).is_ok())
}

fn resolve_git_path(
    runner: &GitRunner,
    root: &Path,
    name: &'static str,
) -> Result<PathBuf, OrbitError> {
    let output = runner.run(
        Some(root),
        "check_mutation_state",
        [
            "--no-pager",
            "--no-lazy-fetch",
            "--no-optional-locks",
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            name,
        ],
        MUTATION_PROBE_LIMIT,
    )?;
    let output = runner.require_success("check_mutation_state", output)?;
    let path = output.stdout.strip_suffix(b"\n").ok_or_else(|| {
        OrbitError::unsupported(
            "check_mutation_state",
            "Git returned malformed operation-state path data.",
        )
    })?;
    if path.is_empty() || path.contains(&0) {
        return Err(OrbitError::unsupported(
            "check_mutation_state",
            "Git returned malformed operation-state path data.",
        ));
    }
    raw_path(path)
}

fn operation_in_progress() -> OrbitError {
    OrbitError::mutation_not_applicable(
        "Finish or abort the current merge, rebase, cherry-pick, revert, squash, or sequencer operation before using Orbit staging.",
    )
}

fn raw_argument(path: &[u8]) -> Result<OsString, OrbitError> {
    #[cfg(unix)]
    {
        Ok(OsString::from_vec(path.to_vec()))
    }
    #[cfg(not(unix))]
    {
        std::str::from_utf8(path).map(OsString::from).map_err(|_| {
            OrbitError::unsupported(
                "mutate_repository",
                "This platform cannot represent the selected repository path.",
            )
        })
    }
}

fn raw_path(path: &[u8]) -> Result<PathBuf, OrbitError> {
    raw_argument(path).map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_file_arguments_keep_literal_mode_global_and_paths_after_separator() {
        let mut args = stage_arguments(true);
        args.push(OsString::from("--"));
        args.push(OsString::from("*.txt"));
        let args = args
            .iter()
            .map(|value| value.to_string_lossy())
            .collect::<Vec<_>>();

        let literal = args
            .iter()
            .position(|value| value == "--literal-pathspecs")
            .expect("literal mode");
        let add = args.iter().position(|value| value == "add").expect("add");
        let separator = args
            .iter()
            .position(|value| value == "--")
            .expect("separator");
        assert!(literal < add && add < separator);
        assert_eq!(args[separator + 1], "*.txt");
        assert!(args.iter().any(|value| value == "add.ignoreErrors=false"));
        for required in [
            "core.fsmonitor=false",
            "--no-pager",
            "--no-lazy-fetch",
            "--no-optional-locks",
        ] {
            assert!(args.iter().any(|value| value == required), "{required}");
        }
    }

    #[test]
    fn unstage_prefix_disables_submodule_recursion() {
        let args = unstage_prefix(false);
        assert!(args.iter().any(|value| value == "submodule.recurse=false"));
    }
}
