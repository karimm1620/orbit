use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;

use serde::{Deserialize, Serialize};

use crate::error::OrbitError;

use super::{
    status::{append_filter_driver_overrides, configured_filter_drivers, ChangeKind},
    ChangeFacet, ConflictKind, GitRunner, StatusEntry, SubmoduleState,
};

const DIFF_OUTPUT_LIMIT: usize = 4 * 1024 * 1024;
const MAX_DIFF_HUNKS: usize = 10_000;
const MAX_DIFF_LINES: usize = 100_000;
const DIFF_DEADLINE: Duration = Duration::from_secs(30);
const DIFF_OPERATION: &str = "read_file_diff";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DiffSide {
    Staged,
    Unstaged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiffSelection {
    pub path: Vec<u8>,
    pub original_path: Option<Vec<u8>>,
    pub staged: Option<ChangeFacet>,
    pub unstaged: Option<ChangeFacet>,
    pub conflict: Option<ConflictKind>,
    pub submodule: Option<SubmoduleState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDiff {
    pub change_set_id: String,
    pub file_id: String,
    pub side: DiffSide,
    pub content: FileDiffContent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum FileDiffContent {
    Text {
        additions: u64,
        deletions: u64,
        metadata: DiffMetadata,
        hunks: Vec<DiffHunk>,
    },
    Binary,
    Conflict {
        kind: ConflictKind,
    },
    Submodule {
        submodule: SubmoduleState,
    },
    TooLarge,
    Unavailable {
        reason: DiffUnavailableReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DiffUnavailableReason {
    Stale,
    SideUnavailable,
    MissingFile,
    UnreadableFile,
    SpecialFile,
    UnsupportedEncoding,
    Timeout,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffMetadata {
    pub old_mode: Option<String>,
    pub new_mode: Option<String>,
    pub new_file: bool,
    pub deleted_file: bool,
    pub renamed: bool,
    pub copied: bool,
    pub similarity: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffHunk {
    pub old_start: u64,
    pub old_count: u64,
    pub new_start: u64,
    pub new_count: u64,
    pub heading: Option<String>,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DiffLineKind {
    Context,
    Addition,
    Deletion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub old_line: Option<u64>,
    pub new_line: Option<u64>,
    pub content: String,
    pub no_newline_at_end: bool,
}

pub(crate) fn read_file_diff(
    runner: &GitRunner,
    root: &Path,
    selection: &DiffSelection,
    fresh_entries: &[StatusEntry],
    change_set_id: &str,
    file_id: &str,
    side: DiffSide,
) -> Result<FileDiff, OrbitError> {
    let wrap = |content| FileDiff {
        change_set_id: change_set_id.to_owned(),
        file_id: file_id.to_owned(),
        side,
        content,
    };

    let Some(fresh) = fresh_entries
        .iter()
        .find(|entry| entry.path == selection.path)
    else {
        return Ok(wrap(FileDiffContent::Unavailable {
            reason: DiffUnavailableReason::Stale,
        }));
    };

    if let Some(kind) = selection.conflict {
        return Ok(if fresh.conflict == Some(kind) {
            wrap(FileDiffContent::Conflict { kind })
        } else {
            wrap(FileDiffContent::Unavailable {
                reason: DiffUnavailableReason::Stale,
            })
        });
    }

    let selected_facet = facet_for_side(selection, side);
    let fresh_facet = facet_for_status(fresh, side);
    let (Some(selected_facet), Some(fresh_facet)) = (selected_facet, fresh_facet) else {
        return Ok(wrap(FileDiffContent::Unavailable {
            reason: DiffUnavailableReason::SideUnavailable,
        }));
    };
    if selected_facet != fresh_facet
        || selection.original_path != fresh.original_path
        || selection.submodule != fresh.submodule
    {
        return Ok(wrap(FileDiffContent::Unavailable {
            reason: DiffUnavailableReason::Stale,
        }));
    }

    if selected_facet.old_mode.as_deref() == Some("160000")
        || selected_facet.new_mode.as_deref() == Some("160000")
        || selection.submodule.is_some()
    {
        return Ok(wrap(FileDiffContent::Submodule {
            submodule: selection.submodule.unwrap_or(SubmoduleState {
                commit_changed: true,
                tracked_changes: false,
                untracked_changes: false,
            }),
        }));
    }

    let mode = if selected_facet.kind == ChangeKind::Untracked {
        DiffMode::Untracked
    } else if side == DiffSide::Staged {
        DiffMode::Staged
    } else {
        DiffMode::Unstaged
    };

    if matches!(mode, DiffMode::Untracked)
        || (matches!(mode, DiffMode::Unstaged) && selected_facet.kind != ChangeKind::Deleted)
    {
        if let Some(reason) = validate_worktree_file(root, &selection.path)? {
            return Ok(wrap(FileDiffContent::Unavailable { reason }));
        }
    }

    let movement = matches!(
        selected_facet.kind,
        ChangeKind::Renamed | ChangeKind::Copied
    );
    let path_arguments = diff_paths(mode, selection, movement)?;
    let expected_identity = expected_identity(mode, selection, movement)?;
    let filter_drivers = configured_filter_drivers(runner, root)?;
    let args = diff_arguments(&filter_drivers, mode, selected_facet.kind, &path_arguments);
    let output = match runner.run_with_deadline(
        Some(root),
        DIFF_OPERATION,
        args,
        DIFF_OUTPUT_LIMIT,
        DIFF_DEADLINE,
    ) {
        Ok(output) => output,
        Err(error) if error.code == "git_command_timed_out" => {
            return Ok(wrap(FileDiffContent::Unavailable {
                reason: DiffUnavailableReason::Timeout,
            }));
        }
        Err(error) if error.is_output_too_large_for(DIFF_OPERATION) => {
            return Ok(wrap(FileDiffContent::TooLarge));
        }
        Err(error) => return Err(error),
    };

    let expected_success =
        output.status.success() || (mode == DiffMode::Untracked && output.status.code() == Some(1));
    if !expected_success {
        return Err(OrbitError::git_failed(DIFF_OPERATION, &output.stderr));
    }

    let envelope = match parse_diff_envelope(&output.stdout, &expected_identity) {
        Ok(envelope) => envelope,
        Err(EnvelopeError::IdentityMismatch) => {
            return Ok(wrap(FileDiffContent::Unavailable {
                reason: DiffUnavailableReason::Stale,
            }));
        }
        Err(EnvelopeError::Malformed) => return Err(malformed_diff()),
    };
    if envelope.binary {
        return Ok(wrap(FileDiffContent::Binary));
    }
    let patch = match std::str::from_utf8(envelope.patch) {
        Ok(patch) => patch,
        Err(_) => {
            return Ok(wrap(FileDiffContent::Unavailable {
                reason: DiffUnavailableReason::UnsupportedEncoding,
            }));
        }
    };
    let parsed = match parse_patch(patch, envelope.additions, envelope.deletions) {
        Ok(parsed) => parsed,
        Err(error) if error.is_output_too_large_for("parse_file_diff") => {
            return Ok(wrap(FileDiffContent::TooLarge));
        }
        Err(error) => return Err(error),
    };
    Ok(wrap(FileDiffContent::Text {
        additions: envelope.additions,
        deletions: envelope.deletions,
        metadata: parsed.metadata,
        hunks: parsed.hunks,
    }))
}

fn facet_for_side(selection: &DiffSelection, side: DiffSide) -> Option<&ChangeFacet> {
    match side {
        DiffSide::Staged => selection.staged.as_ref(),
        DiffSide::Unstaged => selection.unstaged.as_ref(),
    }
}

fn facet_for_status(entry: &StatusEntry, side: DiffSide) -> Option<&ChangeFacet> {
    match side {
        DiffSide::Staged => entry.staged.as_ref(),
        DiffSide::Unstaged => entry.unstaged.as_ref(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffMode {
    Staged,
    Unstaged,
    Untracked,
}

fn validate_worktree_file(
    root: &Path,
    relative: &[u8],
) -> Result<Option<DiffUnavailableReason>, OrbitError> {
    let path = root.join(raw_path(relative)?);
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() || metadata.file_type().is_symlink() => {
            Ok(None)
        }
        Ok(_) => Ok(Some(DiffUnavailableReason::SpecialFile)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(Some(DiffUnavailableReason::MissingFile))
        }
        Err(_) => Ok(Some(DiffUnavailableReason::UnreadableFile)),
    }
}

fn raw_path(relative: &[u8]) -> Result<PathBuf, OrbitError> {
    #[cfg(unix)]
    {
        Ok(PathBuf::from(OsString::from_vec(relative.to_vec())))
    }
    #[cfg(not(unix))]
    {
        let value = std::str::from_utf8(relative).map_err(|_| {
            OrbitError::unsupported(
                DIFF_OPERATION,
                "This platform cannot represent the selected repository path.",
            )
        })?;
        Ok(PathBuf::from(value))
    }
}

fn raw_argument(relative: &[u8]) -> Result<OsString, OrbitError> {
    #[cfg(unix)]
    {
        Ok(OsString::from_vec(relative.to_vec()))
    }
    #[cfg(not(unix))]
    {
        std::str::from_utf8(relative)
            .map(OsString::from)
            .map_err(|_| {
                OrbitError::unsupported(
                    DIFF_OPERATION,
                    "This platform cannot represent the selected repository path.",
                )
            })
    }
}

fn dot_relative_argument(relative: &[u8]) -> Result<OsString, OrbitError> {
    let mut value = b"./".to_vec();
    value.extend_from_slice(relative);
    raw_argument(&value)
}

fn diff_paths(
    mode: DiffMode,
    selection: &DiffSelection,
    movement: bool,
) -> Result<Vec<OsString>, OrbitError> {
    if mode == DiffMode::Untracked {
        return Ok(vec![
            OsString::from("/dev/null"),
            dot_relative_argument(&selection.path)?,
        ]);
    }
    let mut paths = Vec::with_capacity(if movement { 2 } else { 1 });
    if movement {
        paths.push(raw_argument(
            selection
                .original_path
                .as_deref()
                .ok_or_else(malformed_diff)?,
        )?);
    }
    paths.push(raw_argument(&selection.path)?);
    Ok(paths)
}

enum ExpectedIdentity<'a> {
    Single(&'a [u8]),
    Pair(&'a [u8], Vec<u8>),
}

fn expected_identity<'a>(
    mode: DiffMode,
    selection: &'a DiffSelection,
    movement: bool,
) -> Result<ExpectedIdentity<'a>, OrbitError> {
    if mode == DiffMode::Untracked {
        let mut target = b"./".to_vec();
        target.extend_from_slice(&selection.path);
        return Ok(ExpectedIdentity::Pair(b"/dev/null", target));
    }
    if movement {
        return Ok(ExpectedIdentity::Pair(
            selection
                .original_path
                .as_deref()
                .ok_or_else(malformed_diff)?,
            selection.path.clone(),
        ));
    }
    Ok(ExpectedIdentity::Single(&selection.path))
}

fn diff_arguments(
    filter_drivers: &std::collections::BTreeSet<String>,
    mode: DiffMode,
    selected_kind: ChangeKind,
    paths: &[OsString],
) -> Vec<OsString> {
    let mut args = vec![OsString::from("-c"), OsString::from("core.fsmonitor=false")];
    append_filter_driver_overrides(&mut args, filter_drivers);
    args.extend([
        OsString::from("--no-pager"),
        OsString::from("--no-lazy-fetch"),
        OsString::from("--no-optional-locks"),
        OsString::from("diff"),
    ]);
    if mode == DiffMode::Staged {
        args.push(OsString::from("--cached"));
    } else if mode == DiffMode::Untracked {
        args.push(OsString::from("--no-index"));
    }
    match selected_kind {
        ChangeKind::Renamed => args.push(OsString::from("--diff-filter=R")),
        ChangeKind::Copied => args.push(OsString::from("--diff-filter=C")),
        _ => {}
    }
    args.extend(
        [
            "--numstat",
            "-z",
            "--patch",
            "--full-index",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--unified=3",
            "--diff-algorithm=myers",
            "--find-renames=50%",
            "--find-copies=50%",
            "-l1000",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            "--",
        ]
        .map(OsString::from),
    );
    args.extend(paths.iter().cloned());
    args
}

#[derive(Debug)]
struct DiffEnvelope<'a> {
    additions: u64,
    deletions: u64,
    binary: bool,
    patch: &'a [u8],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EnvelopeError {
    Malformed,
    IdentityMismatch,
}

fn parse_diff_envelope<'a>(
    output: &'a [u8],
    expected: &ExpectedIdentity<'_>,
) -> Result<DiffEnvelope<'a>, EnvelopeError> {
    if output.is_empty() {
        return Err(EnvelopeError::IdentityMismatch);
    }
    let separator = output
        .windows(2)
        .position(|window| window == [0, 0])
        .ok_or(EnvelopeError::Malformed)?;
    let numstat = &output[..=separator];
    let patch = &output[separator + 2..];
    let first_tab = numstat
        .iter()
        .position(|byte| *byte == b'\t')
        .ok_or(EnvelopeError::Malformed)?;
    let second_tab = numstat[first_tab + 1..]
        .iter()
        .position(|byte| *byte == b'\t')
        .map(|index| first_tab + 1 + index)
        .ok_or(EnvelopeError::Malformed)?;
    let added = &numstat[..first_tab];
    let deleted = &numstat[first_tab + 1..second_tab];
    let binary = match (added, deleted) {
        (b"-", b"-") => true,
        (left, right)
            if !left.is_empty()
                && !right.is_empty()
                && left.iter().all(u8::is_ascii_digit)
                && right.iter().all(u8::is_ascii_digit) =>
        {
            false
        }
        _ => return Err(EnvelopeError::Malformed),
    };
    let additions = if binary {
        0
    } else {
        parse_u64(added).map_err(|_| EnvelopeError::Malformed)?
    };
    let deletions = if binary {
        0
    } else {
        parse_u64(deleted).map_err(|_| EnvelopeError::Malformed)?
    };
    let paths = &numstat[second_tab + 1..];
    let identity_matches = match expected {
        ExpectedIdentity::Single(expected) => paths
            .strip_suffix(&[0])
            .is_some_and(|path| path == *expected),
        ExpectedIdentity::Pair(origin, target) => {
            let records = paths.split(|byte| *byte == 0).collect::<Vec<_>>();
            records.as_slice() == [b"".as_slice(), *origin, target.as_slice(), b"".as_slice()]
        }
    };
    if !identity_matches {
        return Err(EnvelopeError::IdentityMismatch);
    }

    Ok(DiffEnvelope {
        additions,
        deletions,
        binary,
        patch,
    })
}

#[derive(Debug)]
struct ParsedPatch {
    metadata: DiffMetadata,
    hunks: Vec<DiffHunk>,
}

fn parse_patch(
    patch: &str,
    expected_additions: u64,
    expected_deletions: u64,
) -> Result<ParsedPatch, OrbitError> {
    let lines = patch.split_terminator('\n').collect::<Vec<_>>();
    let mut metadata = DiffMetadata::default();
    let mut hunks = Vec::new();
    let mut index = 0;
    let mut file_headers = 0_usize;
    let mut additions = 0_u64;
    let mut deletions = 0_u64;
    let mut total_lines = 0_usize;

    while index < lines.len() {
        let line = lines[index];
        if line.starts_with("diff --git ") {
            file_headers += 1;
            if file_headers > 1 {
                return Err(malformed_diff());
            }
            index += 1;
            continue;
        }
        if line.starts_with("@@ ") {
            if hunks.len() >= MAX_DIFF_HUNKS {
                return Err(OrbitError::output_too_large("parse_file_diff"));
            }
            let (old_start, old_count, new_start, new_count, heading) = parse_hunk_header(line)?;
            index += 1;
            let mut old_line = old_start;
            let mut new_line = new_start;
            let mut old_seen = 0_u64;
            let mut new_seen = 0_u64;
            let mut hunk_lines: Vec<DiffLine> = Vec::new();

            while index < lines.len() {
                let line = lines[index];
                if old_seen == old_count && new_seen == new_count {
                    break;
                }
                if line == "\\ No newline at end of file" {
                    let previous = hunk_lines.last_mut().ok_or_else(malformed_diff)?;
                    if previous.no_newline_at_end {
                        return Err(malformed_diff());
                    }
                    previous.no_newline_at_end = true;
                    index += 1;
                    continue;
                }
                if total_lines >= MAX_DIFF_LINES {
                    return Err(OrbitError::output_too_large("parse_file_diff"));
                }
                let (kind, content, old_number, new_number) = match line.as_bytes().split_first() {
                    Some((b' ', content)) => {
                        old_seen = old_seen.checked_add(1).ok_or_else(malformed_diff)?;
                        new_seen = new_seen.checked_add(1).ok_or_else(malformed_diff)?;
                        let numbers = (Some(old_line), Some(new_line));
                        old_line = old_line.checked_add(1).ok_or_else(malformed_diff)?;
                        new_line = new_line.checked_add(1).ok_or_else(malformed_diff)?;
                        (DiffLineKind::Context, content, numbers.0, numbers.1)
                    }
                    Some((b'+', content)) => {
                        new_seen = new_seen.checked_add(1).ok_or_else(malformed_diff)?;
                        let number = new_line;
                        new_line = new_line.checked_add(1).ok_or_else(malformed_diff)?;
                        additions = additions.checked_add(1).ok_or_else(malformed_diff)?;
                        (DiffLineKind::Addition, content, None, Some(number))
                    }
                    Some((b'-', content)) => {
                        old_seen = old_seen.checked_add(1).ok_or_else(malformed_diff)?;
                        let number = old_line;
                        old_line = old_line.checked_add(1).ok_or_else(malformed_diff)?;
                        deletions = deletions.checked_add(1).ok_or_else(malformed_diff)?;
                        (DiffLineKind::Deletion, content, Some(number), None)
                    }
                    _ => return Err(malformed_diff()),
                };
                if old_seen > old_count || new_seen > new_count {
                    return Err(malformed_diff());
                }
                hunk_lines.push(DiffLine {
                    kind,
                    old_line: old_number,
                    new_line: new_number,
                    content: std::str::from_utf8(content)
                        .map_err(|_| malformed_diff())?
                        .to_owned(),
                    no_newline_at_end: false,
                });
                total_lines += 1;
                index += 1;
            }
            if old_seen != old_count || new_seen != new_count {
                return Err(malformed_diff());
            }
            if index < lines.len() && lines[index] == "\\ No newline at end of file" {
                let previous = hunk_lines.last_mut().ok_or_else(malformed_diff)?;
                if previous.no_newline_at_end {
                    return Err(malformed_diff());
                }
                previous.no_newline_at_end = true;
                index += 1;
            }
            hunks.push(DiffHunk {
                old_start,
                old_count,
                new_start,
                new_count,
                heading,
                lines: hunk_lines,
            });
            continue;
        }
        parse_metadata_line(line, &mut metadata)?;
        index += 1;
    }

    if file_headers != 1 || additions != expected_additions || deletions != expected_deletions {
        return Err(malformed_diff());
    }
    Ok(ParsedPatch { metadata, hunks })
}

fn parse_hunk_header(line: &str) -> Result<(u64, u64, u64, u64, Option<String>), OrbitError> {
    let rest = line.strip_prefix("@@ -").ok_or_else(malformed_diff)?;
    let (old, rest) = rest.split_once(" +").ok_or_else(malformed_diff)?;
    let (new, heading) = rest.split_once(" @@").ok_or_else(malformed_diff)?;
    let (old_start, old_count) = parse_range(old)?;
    let (new_start, new_count) = parse_range(new)?;
    let heading = heading.strip_prefix(' ').unwrap_or(heading).to_owned();
    Ok((
        old_start,
        old_count,
        new_start,
        new_count,
        (!heading.is_empty()).then_some(heading),
    ))
}

fn parse_range(value: &str) -> Result<(u64, u64), OrbitError> {
    let (start, count) = value
        .split_once(',')
        .map_or((value, "1"), |(start, count)| (start, count));
    if start.is_empty()
        || count.is_empty()
        || !start.bytes().all(|byte| byte.is_ascii_digit())
        || !count.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(malformed_diff());
    }
    Ok((parse_u64(start.as_bytes())?, parse_u64(count.as_bytes())?))
}

fn parse_u64(value: &[u8]) -> Result<u64, OrbitError> {
    std::str::from_utf8(value)
        .map_err(|_| malformed_diff())?
        .parse()
        .map_err(|_| malformed_diff())
}

fn parse_metadata_line(line: &str, metadata: &mut DiffMetadata) -> Result<(), OrbitError> {
    if line.starts_with("index ")
        || line.starts_with("--- ")
        || line.starts_with("+++ ")
        || line.starts_with("rename from ")
        || line.starts_with("rename to ")
        || line.starts_with("copy from ")
        || line.starts_with("copy to ")
        || line.starts_with("Binary files ")
    {
        metadata.renamed |= line.starts_with("rename ");
        metadata.copied |= line.starts_with("copy ");
        return Ok(());
    }
    if let Some(mode) = line.strip_prefix("new file mode ") {
        validate_patch_mode(mode)?;
        metadata.new_file = true;
        metadata.new_mode = Some(mode.to_owned());
        return Ok(());
    }
    if let Some(mode) = line.strip_prefix("deleted file mode ") {
        validate_patch_mode(mode)?;
        metadata.deleted_file = true;
        metadata.old_mode = Some(mode.to_owned());
        return Ok(());
    }
    if let Some(mode) = line.strip_prefix("old mode ") {
        validate_patch_mode(mode)?;
        metadata.old_mode = Some(mode.to_owned());
        return Ok(());
    }
    if let Some(mode) = line.strip_prefix("new mode ") {
        validate_patch_mode(mode)?;
        metadata.new_mode = Some(mode.to_owned());
        return Ok(());
    }
    if let Some(value) = line.strip_prefix("similarity index ") {
        let value = value.strip_suffix('%').ok_or_else(malformed_diff)?;
        let similarity = value.parse::<u8>().map_err(|_| malformed_diff())?;
        if similarity > 100 {
            return Err(malformed_diff());
        }
        metadata.similarity = Some(similarity);
        return Ok(());
    }
    if line.starts_with("dissimilarity index ") {
        return Ok(());
    }
    Err(malformed_diff())
}

fn validate_patch_mode(mode: &str) -> Result<(), OrbitError> {
    if mode.len() == 6 && mode.bytes().all(|byte| matches!(byte, b'0'..=b'7')) {
        Ok(())
    } else {
        Err(malformed_diff())
    }
}

fn malformed_diff() -> OrbitError {
    OrbitError::unsupported(
        "parse_file_diff",
        "Git returned malformed selected-file diff data.",
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn builds_a_fully_hardened_copy_diff_command() {
        let drivers = BTreeSet::from(["orbit".to_owned()]);
        let paths = vec![OsString::from("old"), OsString::from("new")];
        let args = diff_arguments(&drivers, DiffMode::Staged, ChangeKind::Copied, &paths);
        let args = args
            .iter()
            .map(|argument| argument.to_string_lossy())
            .collect::<Vec<_>>();

        for required in [
            "core.fsmonitor=false",
            "filter.orbit.clean=",
            "filter.orbit.process=",
            "filter.orbit.required=false",
            "--no-pager",
            "--no-lazy-fetch",
            "--no-optional-locks",
            "--cached",
            "--diff-filter=C",
            "--numstat",
            "-z",
            "--patch",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--diff-algorithm=myers",
            "--find-renames=50%",
            "--find-copies=50%",
            "-l1000",
            "--",
        ] {
            assert!(args.iter().any(|argument| argument == required));
        }
        let separator = args
            .iter()
            .position(|argument| argument == "--")
            .expect("path separator");
        assert_eq!(&args[separator + 1..], ["old", "new"]);
    }

    #[test]
    fn parses_single_and_pair_numstat_framing() {
        let output = b"1\t2\tfile.txt\0\0diff --git a/file.txt b/file.txt\n";
        let parsed = parse_diff_envelope(output, &ExpectedIdentity::Single(b"file.txt"))
            .expect("single-path envelope");
        assert_eq!((parsed.additions, parsed.deletions), (1, 2));
        assert_eq!(parsed.patch, b"diff --git a/file.txt b/file.txt\n");

        let output = b"0\t0\t\0old name\0new\tname\0\0patch";
        let parsed = parse_diff_envelope(
            output,
            &ExpectedIdentity::Pair(b"old name", b"new\tname".to_vec()),
        )
        .expect("rename envelope");
        assert_eq!((parsed.additions, parsed.deletions), (0, 0));
        assert_eq!(parsed.patch, b"patch");
    }

    #[test]
    fn classifies_binary_numstat_without_reading_patch_prose() {
        let output = b"-\t-\tbinary.dat\0\0arbitrary non-UTF-8: \xff";
        let parsed = parse_diff_envelope(output, &ExpectedIdentity::Single(b"binary.dat"))
            .expect("binary envelope");

        assert!(parsed.binary);
        assert_eq!((parsed.additions, parsed.deletions), (0, 0));
    }

    #[test]
    fn rejects_malformed_or_spoofed_numstat_identity() {
        assert_eq!(
            parse_diff_envelope(b"", &ExpectedIdentity::Single(b"file.txt"))
                .expect_err("missing selected entry"),
            EnvelopeError::IdentityMismatch
        );
        assert_eq!(
            parse_diff_envelope(
                b"1\t0\tother.txt\0\0patch",
                &ExpectedIdentity::Single(b"file.txt")
            )
            .expect_err("path mismatch"),
            EnvelopeError::IdentityMismatch
        );
        assert_eq!(
            parse_diff_envelope(
                b"1\t-\tfile.txt\0\0patch",
                &ExpectedIdentity::Single(b"file.txt")
            )
            .expect_err("mixed binary framing"),
            EnvelopeError::Malformed
        );
        assert_eq!(
            parse_diff_envelope(
                b"1\t0\tfile.txt\0patch",
                &ExpectedIdentity::Single(b"file.txt")
            )
            .expect_err("missing transition"),
            EnvelopeError::Malformed
        );
    }

    #[test]
    fn parses_hunks_line_numbers_unicode_heading_and_no_newline_markers() {
        let patch = concat!(
            "diff --git a/file.txt b/file.txt\n",
            "index 111..222 100644\n",
            "--- a/file.txt\n",
            "+++ b/file.txt\n",
            "@@ -1,2 +1,2 @@ función\n",
            "-old\n",
            "\\ No newline at end of file\n",
            "+新\n",
            "\\ No newline at end of file\n",
            " same\n",
        );
        let parsed = parse_patch(patch, 1, 1).expect("valid patch");
        let hunk = &parsed.hunks[0];

        assert_eq!(hunk.heading.as_deref(), Some("función"));
        assert_eq!(hunk.lines.len(), 3);
        assert_eq!(
            (hunk.lines[0].old_line, hunk.lines[0].new_line),
            (Some(1), None)
        );
        assert!(hunk.lines[0].no_newline_at_end);
        assert_eq!(hunk.lines[1].content, "新");
        assert_eq!(
            (hunk.lines[1].old_line, hunk.lines[1].new_line),
            (None, Some(1))
        );
        assert!(hunk.lines[1].no_newline_at_end);
        assert_eq!(
            (hunk.lines[2].old_line, hunk.lines[2].new_line),
            (Some(2), Some(2))
        );
    }

    #[test]
    fn parses_omitted_counts_multiple_hunks_and_metadata_only_changes() {
        let patch = "diff --git a/file b/file\n\
                     old mode 100644\n\
                     new mode 100755\n\
                     --- a/file\n\
                     +++ b/file\n\
                     @@ -1 +1 @@\n\
                     -a\n\
                     +b\n\
                     @@ -3 +3 @@ tail\n\
                     -c\n\
                     +d\n";
        let parsed = parse_patch(patch, 2, 2).expect("multiple hunks");
        assert_eq!(parsed.hunks.len(), 2);
        assert_eq!(parsed.hunks[0].old_count, 1);
        assert_eq!(parsed.metadata.old_mode.as_deref(), Some("100644"));
        assert_eq!(parsed.metadata.new_mode.as_deref(), Some("100755"));

        let rename = "diff --git a/old b/new\n\
                      similarity index 100%\n\
                      rename from old\n\
                      rename to new\n";
        let parsed = parse_patch(rename, 0, 0).expect("rename-only metadata");
        assert!(parsed.hunks.is_empty());
        assert!(parsed.metadata.renamed);
        assert_eq!(parsed.metadata.similarity, Some(100));
    }

    #[test]
    fn rejects_malformed_hunks_and_accounting() {
        let malformed = "diff --git a/file b/file\n@@ -x +1 @@\n-a\n+b\n";
        assert!(parse_patch(malformed, 1, 1).is_err());

        let incomplete = "diff --git a/file b/file\n@@ -1,2 +1 @@\n-a\n+b\n";
        assert!(parse_patch(incomplete, 1, 1).is_err());

        let wrong_numstat = "diff --git a/file b/file\n@@ -1 +1 @@\n-a\n+b\n";
        assert!(parse_patch(wrong_numstat, 2, 1).is_err());
    }

    #[test]
    fn enforces_hunk_and_line_collection_bounds() {
        let mut too_many_hunks = String::from("diff --git a/file b/file\n");
        for _ in 0..=MAX_DIFF_HUNKS {
            too_many_hunks.push_str("@@ -0,0 +0,0 @@\n");
        }
        let error = parse_patch(&too_many_hunks, 0, 0).expect_err("hunk bound");
        assert!(error.is_output_too_large_for("parse_file_diff"));

        let mut too_many_lines = format!(
            "diff --git a/file b/file\n@@ -0,0 +1,{} @@\n",
            MAX_DIFF_LINES + 1
        );
        for _ in 0..=MAX_DIFF_LINES {
            too_many_lines.push_str("+x\n");
        }
        let error =
            parse_patch(&too_many_lines, (MAX_DIFF_LINES + 1) as u64, 0).expect_err("line bound");
        assert!(error.is_output_too_large_for("parse_file_diff"));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_fifo_before_starting_git() {
        use std::{
            process::Command,
            time::{SystemTime, UNIX_EPOCH},
        };

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("orbit-diff-fifo-{}-{nonce}", std::process::id()));
        fs::create_dir(&root).expect("create FIFO fixture directory");
        let fifo = root.join("pipe");
        let status = Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("run mkfifo");
        assert!(status.success());

        let reason = validate_worktree_file(&root, b"pipe")
            .expect("inspect FIFO")
            .expect("FIFO should be unavailable");
        assert_eq!(reason, DiffUnavailableReason::SpecialFile);
        fs::remove_dir_all(root).expect("remove FIFO fixture");
    }

    #[test]
    fn reports_a_missing_worktree_file_without_following_any_path() {
        let root = std::env::temp_dir();
        let reason = validate_worktree_file(&root, b"orbit-file-that-does-not-exist")
            .expect("inspect missing file")
            .expect("missing file should be unavailable");
        assert_eq!(reason, DiffUnavailableReason::MissingFile);
    }
}
