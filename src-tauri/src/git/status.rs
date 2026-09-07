use std::{
    collections::{btree_map::Entry, BTreeMap, BTreeSet},
    ffi::OsString,
    path::Path,
};

use serde::Serialize;

use crate::error::OrbitError;

use super::GitRunner;

const STATUS_OUTPUT_LIMIT: usize = 16 * 1024 * 1024;
const CONFIG_OUTPUT_LIMIT: usize = 1024 * 1024;
const MAX_FILTER_DRIVERS: usize = 256;
const MAX_FILTER_DRIVER_BYTES: usize = 1024;
pub(crate) const MAX_CHANGE_ENTRIES: usize = 10_000;
pub(crate) const MAX_CHANGE_PATH_BYTES: usize = 16 * 1024;
pub(crate) const MAX_CHANGE_SET_PATH_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HeadSnapshot {
    pub oid: Option<String>,
    pub branch: Option<String>,
    pub detached: bool,
    pub upstream: Option<String>,
    pub ahead: Option<u64>,
    pub behind: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkingTreeSnapshot {
    pub staged: u64,
    pub unstaged: u64,
    pub untracked: u64,
    pub conflicted: u64,
    pub clean: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Untracked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeFacet {
    pub kind: ChangeKind,
    pub old_mode: Option<String>,
    pub new_mode: Option<String>,
    pub similarity: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ConflictKind {
    BothDeleted,
    AddedByUs,
    DeletedByThem,
    AddedByThem,
    DeletedByUs,
    BothAdded,
    BothModified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmoduleState {
    pub commit_changed: bool,
    pub tracked_changes: bool,
    pub untracked_changes: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StatusEntry {
    pub path: Vec<u8>,
    pub original_path: Option<Vec<u8>>,
    pub staged: Option<ChangeFacet>,
    pub unstaged: Option<ChangeFacet>,
    pub conflict: Option<ConflictKind>,
    pub submodule: Option<SubmoduleState>,
}

#[derive(Debug)]
pub struct StatusSnapshot {
    pub head: HeadSnapshot,
    pub working_tree: WorkingTreeSnapshot,
    pub(crate) changes: Vec<StatusEntry>,
}

pub fn read_status(runner: &GitRunner, root: &Path) -> Result<StatusSnapshot, OrbitError> {
    read_status_with_policy(runner, root, false)
}

pub(crate) fn read_detailed_status(
    runner: &GitRunner,
    root: &Path,
) -> Result<StatusSnapshot, OrbitError> {
    runner.require_no_lazy_fetch()?;
    read_status_with_policy(runner, root, true)
}

fn read_status_with_policy(
    runner: &GitRunner,
    root: &Path,
    require_no_lazy_fetch: bool,
) -> Result<StatusSnapshot, OrbitError> {
    let filter_drivers = configured_filter_drivers(runner, root)?;
    let args = status_arguments(&filter_drivers, require_no_lazy_fetch);
    let operation = if require_no_lazy_fetch {
        "read_repository_changes"
    } else {
        "read_status"
    };
    let output = runner.run(Some(root), operation, args, STATUS_OUTPUT_LIMIT)?;
    let output = runner.require_success(operation, output)?;

    parse_status(&output.stdout)
}

fn status_arguments(
    filter_drivers: &BTreeSet<String>,
    require_no_lazy_fetch: bool,
) -> Vec<OsString> {
    let mut args = Vec::with_capacity(24 + filter_drivers.len() * 6);
    args.extend([
        OsString::from("-c"),
        OsString::from("core.fsmonitor=false"),
        OsString::from("-c"),
        OsString::from("status.renames=copies"),
        OsString::from("-c"),
        OsString::from("diff.renameLimit=1000"),
    ]);
    for driver in filter_drivers {
        args.extend([
            OsString::from("-c"),
            OsString::from(format!("filter.{driver}.clean=")),
            OsString::from("-c"),
            OsString::from(format!("filter.{driver}.process=")),
            OsString::from("-c"),
            OsString::from(format!("filter.{driver}.required=false")),
        ]);
    }
    args.push(OsString::from("--no-pager"));
    if require_no_lazy_fetch {
        args.push(OsString::from("--no-lazy-fetch"));
    }
    args.extend(
        [
            "--no-optional-locks",
            "status",
            "--porcelain=v2",
            "--branch",
            "--untracked-files=all",
            "--ignore-submodules=dirty",
            "--find-renames=50%",
            "-z",
        ]
        .map(OsString::from),
    );
    args
}

fn configured_filter_drivers(
    runner: &GitRunner,
    root: &Path,
) -> Result<BTreeSet<String>, OrbitError> {
    let output = runner.run(
        Some(root),
        "read_git_configuration",
        [
            "--no-optional-locks",
            "config",
            "--null",
            "--name-only",
            "--list",
        ],
        CONFIG_OUTPUT_LIMIT,
    )?;
    let output = runner.require_success("read_git_configuration", output)?;
    parse_filter_drivers(&output.stdout)
}

fn parse_filter_drivers(output: &[u8]) -> Result<BTreeSet<String>, OrbitError> {
    let mut drivers = BTreeSet::new();

    for key in output
        .split(|byte| *byte == 0)
        .filter(|key| !key.is_empty())
    {
        let suffix_length = if key.len() > b"filter..clean".len()
            && key[..7].eq_ignore_ascii_case(b"filter.")
            && key[key.len() - 6..].eq_ignore_ascii_case(b".clean")
        {
            6
        } else if key.len() > b"filter..process".len()
            && key[..7].eq_ignore_ascii_case(b"filter.")
            && key[key.len() - 8..].eq_ignore_ascii_case(b".process")
        {
            8
        } else {
            continue;
        };
        let driver = &key[7..key.len() - suffix_length];
        if driver.len() > MAX_FILTER_DRIVER_BYTES {
            return Err(OrbitError::output_too_large("read_git_configuration"));
        }
        let driver = std::str::from_utf8(driver).map_err(|_| {
            OrbitError::unsupported(
                "read_git_configuration",
                "A configured Git filter name is not valid UTF-8.",
            )
        })?;
        drivers.insert(driver.to_owned());
        if drivers.len() > MAX_FILTER_DRIVERS {
            return Err(OrbitError::output_too_large("read_git_configuration"));
        }
    }

    Ok(drivers)
}

fn parse_status(output: &[u8]) -> Result<StatusSnapshot, OrbitError> {
    if !output.is_empty() && !output.ends_with(&[0]) {
        return Err(malformed_status());
    }
    let mut head = HeadSnapshot {
        oid: None,
        branch: None,
        detached: false,
        upstream: None,
        ahead: None,
        behind: None,
    };
    let mut changes = BTreeMap::<Vec<u8>, StatusEntry>::new();
    let records: Vec<&[u8]> = output.split(|byte| *byte == 0).collect();
    let mut index = 0;

    while index < records.len() {
        let record = records[index];
        index += 1;

        if record.is_empty() {
            continue;
        }

        let entry = match record[0] {
            b'#' => {
                parse_header(record, &mut head)?;
                continue;
            }
            b'1' => parse_ordinary_record(record)?,
            b'2' => {
                let original_path = records.get(index).copied().ok_or_else(malformed_status)?;
                index += 1;
                parse_rename_record(record, original_path)?
            }
            b'u' => parse_unmerged_record(record)?,
            b'?' => parse_untracked_record(record)?,
            b'!' => continue,
            _ => {
                return Err(OrbitError::unsupported(
                    "parse_status",
                    "Git returned an unknown porcelain status record.",
                ));
            }
        };

        insert_entry(&mut changes, entry)?;
    }

    let changes = changes.into_values().collect::<Vec<_>>();
    validate_total_path_bytes(&changes)?;
    let working_tree = summarize_changes(&changes);

    Ok(StatusSnapshot {
        head,
        working_tree,
        changes,
    })
}

fn parse_ordinary_record(record: &[u8]) -> Result<StatusEntry, OrbitError> {
    let fields = split_record(record, 9)?;
    if fields[0] != b"1" {
        return Err(malformed_status());
    }
    let (index_status, worktree_status) = parse_xy(fields[1])?;
    let submodule = parse_submodule(fields[2])?;
    validate_modes(&fields[3..=5])?;
    validate_oids(&fields[6..=7])?;
    validate_path(fields[8])?;

    Ok(StatusEntry {
        path: fields[8].to_vec(),
        original_path: None,
        staged: parse_facet(index_status, fields[3], fields[4], None)?,
        unstaged: parse_facet(worktree_status, fields[4], fields[5], None)?,
        conflict: None,
        submodule,
    })
}

fn parse_rename_record(record: &[u8], original_path: &[u8]) -> Result<StatusEntry, OrbitError> {
    let fields = split_record(record, 10)?;
    if fields[0] != b"2" {
        return Err(malformed_status());
    }
    let (index_status, worktree_status) = parse_xy(fields[1])?;
    let submodule = parse_submodule(fields[2])?;
    validate_modes(&fields[3..=5])?;
    validate_oids(&fields[6..=7])?;
    let (movement, similarity) = parse_movement(fields[8])?;
    validate_path(fields[9])?;
    validate_path(original_path)?;

    let staged = parse_facet(
        index_status,
        fields[3],
        fields[4],
        Some((movement, similarity)),
    )?;
    let unstaged = parse_facet(
        worktree_status,
        fields[4],
        fields[5],
        Some((movement, similarity)),
    )?;
    let staged_movement = staged
        .as_ref()
        .is_some_and(|facet| matches!(facet.kind, ChangeKind::Renamed | ChangeKind::Copied));
    let unstaged_movement = unstaged
        .as_ref()
        .is_some_and(|facet| matches!(facet.kind, ChangeKind::Renamed | ChangeKind::Copied));
    if !staged_movement && !unstaged_movement {
        return Err(malformed_status());
    }

    Ok(StatusEntry {
        path: fields[9].to_vec(),
        original_path: Some(original_path.to_vec()),
        staged,
        unstaged,
        conflict: None,
        submodule,
    })
}

fn parse_unmerged_record(record: &[u8]) -> Result<StatusEntry, OrbitError> {
    let fields = split_record(record, 11)?;
    if fields[0] != b"u" {
        return Err(malformed_status());
    }
    let conflict = match fields[1] {
        b"DD" => ConflictKind::BothDeleted,
        b"AU" => ConflictKind::AddedByUs,
        b"UD" => ConflictKind::DeletedByThem,
        b"UA" => ConflictKind::AddedByThem,
        b"DU" => ConflictKind::DeletedByUs,
        b"AA" => ConflictKind::BothAdded,
        b"UU" => ConflictKind::BothModified,
        _ => return Err(malformed_status()),
    };
    let submodule = parse_submodule(fields[2])?;
    validate_modes(&fields[3..=6])?;
    validate_oids(&fields[7..=9])?;
    validate_path(fields[10])?;

    Ok(StatusEntry {
        path: fields[10].to_vec(),
        original_path: None,
        staged: None,
        unstaged: None,
        conflict: Some(conflict),
        submodule,
    })
}

fn parse_untracked_record(record: &[u8]) -> Result<StatusEntry, OrbitError> {
    let path = record.strip_prefix(b"? ").ok_or_else(malformed_status)?;
    validate_path(path)?;

    Ok(StatusEntry {
        path: path.to_vec(),
        original_path: None,
        staged: None,
        unstaged: Some(ChangeFacet {
            kind: ChangeKind::Untracked,
            old_mode: None,
            new_mode: None,
            similarity: None,
        }),
        conflict: None,
        submodule: None,
    })
}

fn split_record(record: &[u8], fields: usize) -> Result<Vec<&[u8]>, OrbitError> {
    let parts = record
        .splitn(fields, |byte| *byte == b' ')
        .collect::<Vec<_>>();
    if parts.len() != fields || parts.iter().any(|part| part.is_empty()) {
        return Err(malformed_status());
    }
    Ok(parts)
}

fn parse_xy(value: &[u8]) -> Result<(u8, u8), OrbitError> {
    if value.len() != 2 {
        return Err(malformed_status());
    }
    Ok((value[0], value[1]))
}

fn parse_facet(
    status: u8,
    old_mode: &[u8],
    new_mode: &[u8],
    movement: Option<(ChangeKind, u8)>,
) -> Result<Option<ChangeFacet>, OrbitError> {
    if status == b'.' {
        return Ok(None);
    }
    let kind = match status {
        b'A' => ChangeKind::Added,
        b'M' => ChangeKind::Modified,
        b'D' => ChangeKind::Deleted,
        b'T' => ChangeKind::TypeChanged,
        b'R' if movement.is_some_and(|(kind, _)| kind == ChangeKind::Renamed) => {
            ChangeKind::Renamed
        }
        b'C' if movement.is_some_and(|(kind, _)| kind == ChangeKind::Copied) => ChangeKind::Copied,
        _ => return Err(malformed_status()),
    };
    let similarity = matches!(kind, ChangeKind::Renamed | ChangeKind::Copied)
        .then(|| movement.expect("movement kind checked").1);

    Ok(Some(ChangeFacet {
        kind,
        old_mode: display_mode(old_mode),
        new_mode: display_mode(new_mode),
        similarity,
    }))
}

fn parse_movement(value: &[u8]) -> Result<(ChangeKind, u8), OrbitError> {
    let (&kind, score) = value.split_first().ok_or_else(malformed_status)?;
    let kind = match kind {
        b'R' => ChangeKind::Renamed,
        b'C' => ChangeKind::Copied,
        _ => return Err(malformed_status()),
    };
    let score = std::str::from_utf8(score)
        .map_err(|_| malformed_status())?
        .parse::<u8>()
        .map_err(|_| malformed_status())?;
    if score > 100 {
        return Err(malformed_status());
    }
    Ok((kind, score))
}

fn parse_submodule(value: &[u8]) -> Result<Option<SubmoduleState>, OrbitError> {
    match value {
        b"N..." => Ok(None),
        [b'S', commit, tracked, untracked]
            if matches!(commit, b'.' | b'C')
                && matches!(tracked, b'.' | b'M')
                && matches!(untracked, b'.' | b'U') =>
        {
            Ok(Some(SubmoduleState {
                commit_changed: *commit == b'C',
                tracked_changes: *tracked == b'M',
                untracked_changes: *untracked == b'U',
            }))
        }
        _ => Err(malformed_status()),
    }
}

fn validate_modes(values: &[&[u8]]) -> Result<(), OrbitError> {
    if values
        .iter()
        .any(|value| value.len() != 6 || !value.iter().all(|byte| matches!(byte, b'0'..=b'7')))
    {
        return Err(malformed_status());
    }
    Ok(())
}

fn validate_oids(values: &[&[u8]]) -> Result<(), OrbitError> {
    if values
        .iter()
        .any(|value| !matches!(value.len(), 40 | 64) || !value.iter().all(u8::is_ascii_hexdigit))
    {
        return Err(malformed_status());
    }
    Ok(())
}

fn display_mode(value: &[u8]) -> Option<String> {
    (value != b"000000").then(|| String::from_utf8_lossy(value).into_owned())
}

fn validate_path(path: &[u8]) -> Result<(), OrbitError> {
    if path.len() > MAX_CHANGE_PATH_BYTES {
        return Err(OrbitError::output_too_large("read_repository_changes"));
    }
    if path.is_empty()
        || path.starts_with(b"/")
        || path
            .split(|byte| *byte == b'/')
            .any(|component| component.is_empty() || matches!(component, b"." | b".."))
    {
        return Err(malformed_status());
    }
    Ok(())
}

fn validate_total_path_bytes(changes: &[StatusEntry]) -> Result<(), OrbitError> {
    let total = changes.iter().try_fold(0_usize, |total, change| {
        total
            .checked_add(change.path.len())
            .and_then(|total| {
                change
                    .original_path
                    .as_ref()
                    .map_or(Some(total), |path| total.checked_add(path.len()))
            })
            .ok_or_else(|| OrbitError::output_too_large("read_repository_changes"))
    })?;
    if total > MAX_CHANGE_SET_PATH_BYTES {
        return Err(OrbitError::output_too_large("read_repository_changes"));
    }
    Ok(())
}

fn insert_entry(
    changes: &mut BTreeMap<Vec<u8>, StatusEntry>,
    incoming: StatusEntry,
) -> Result<(), OrbitError> {
    if !changes.contains_key(&incoming.path) && changes.len() >= MAX_CHANGE_ENTRIES {
        return Err(OrbitError::output_too_large("read_repository_changes"));
    }
    match changes.entry(incoming.path.clone()) {
        Entry::Vacant(entry) => {
            entry.insert(incoming);
        }
        Entry::Occupied(mut entry) => merge_entry(entry.get_mut(), incoming)?,
    }
    Ok(())
}

fn merge_entry(existing: &mut StatusEntry, incoming: StatusEntry) -> Result<(), OrbitError> {
    if existing.conflict.is_some()
        || incoming.conflict.is_some()
        || (existing.staged.is_some() && incoming.staged.is_some())
        || (existing.unstaged.is_some() && incoming.unstaged.is_some())
        || !compatible_optional(&existing.original_path, &incoming.original_path)
        || !compatible_optional(&existing.submodule, &incoming.submodule)
    {
        return Err(malformed_status());
    }

    existing.staged = existing.staged.take().or(incoming.staged);
    existing.unstaged = existing.unstaged.take().or(incoming.unstaged);
    existing.original_path = existing.original_path.take().or(incoming.original_path);
    existing.submodule = existing.submodule.take().or(incoming.submodule);
    Ok(())
}

fn compatible_optional<T: PartialEq>(left: &Option<T>, right: &Option<T>) -> bool {
    left.is_none() || right.is_none() || left == right
}

fn summarize_changes(changes: &[StatusEntry]) -> WorkingTreeSnapshot {
    let staged = changes
        .iter()
        .filter(|change| change.staged.is_some())
        .count() as u64;
    let unstaged = changes
        .iter()
        .filter(|change| {
            change
                .unstaged
                .as_ref()
                .is_some_and(|facet| facet.kind != ChangeKind::Untracked)
        })
        .count() as u64;
    let untracked = changes
        .iter()
        .filter(|change| {
            change
                .unstaged
                .as_ref()
                .is_some_and(|facet| facet.kind == ChangeKind::Untracked)
        })
        .count() as u64;
    let conflicted = changes
        .iter()
        .filter(|change| change.conflict.is_some())
        .count() as u64;

    WorkingTreeSnapshot {
        staged,
        unstaged,
        untracked,
        conflicted,
        clean: staged == 0 && unstaged == 0 && untracked == 0 && conflicted == 0,
    }
}

fn parse_header(record: &[u8], head: &mut HeadSnapshot) -> Result<(), OrbitError> {
    const OID: &[u8] = b"# branch.oid ";
    const BRANCH: &[u8] = b"# branch.head ";
    const UPSTREAM: &[u8] = b"# branch.upstream ";
    const AHEAD_BEHIND: &[u8] = b"# branch.ab ";

    if let Some(value) = record.strip_prefix(OID) {
        if value != b"(initial)" {
            validate_oids(&[value])?;
            head.oid = Some(utf8(value, "HEAD object ID")?.to_owned());
        }
    } else if let Some(value) = record.strip_prefix(BRANCH) {
        if value == b"(detached)" {
            head.detached = true;
        } else {
            head.branch = Some(utf8(value, "branch name")?.to_owned());
        }
    } else if let Some(value) = record.strip_prefix(UPSTREAM) {
        head.upstream = Some(utf8(value, "upstream name")?.to_owned());
    } else if let Some(value) = record.strip_prefix(AHEAD_BEHIND) {
        let value = utf8(value, "ahead/behind counts")?;
        let mut counts = value.split_whitespace();
        head.ahead = Some(parse_count(counts.next(), '+')?);
        head.behind = Some(parse_count(counts.next(), '-')?);
        if counts.next().is_some() {
            return Err(malformed_status());
        }
    }

    Ok(())
}

fn parse_count(value: Option<&str>, prefix: char) -> Result<u64, OrbitError> {
    let value = value.ok_or_else(malformed_status)?;
    value
        .strip_prefix(prefix)
        .ok_or_else(malformed_status)?
        .parse()
        .map_err(|_| malformed_status())
}

fn utf8<'a>(value: &'a [u8], field: &str) -> Result<&'a str, OrbitError> {
    std::str::from_utf8(value).map_err(|_| {
        OrbitError::unsupported(
            "parse_status",
            format!("The repository {field} is not valid UTF-8."),
        )
    })
}

fn malformed_status() -> OrbitError {
    OrbitError::unsupported(
        "parse_status",
        "Git returned malformed porcelain v2 status data.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE_OID: &str = "1111111111111111111111111111111111111111";
    const TWO_OID: &str = "2222222222222222222222222222222222222222";
    const THREE_OID: &str = "3333333333333333333333333333333333333333";

    fn ordinary(xy: &str, modes: [&str; 3], path: &[u8]) -> Vec<u8> {
        let mut record = format!(
            "1 {xy} N... {} {} {} {ONE_OID} {TWO_OID} ",
            modes[0], modes[1], modes[2]
        )
        .into_bytes();
        record.extend_from_slice(path);
        record.push(0);
        record
    }

    fn unmerged(xy: &str, path: &[u8]) -> Vec<u8> {
        let mut record =
            format!("u {xy} N... 100644 100644 100644 100644 {ONE_OID} {TWO_OID} {THREE_OID} ")
                .into_bytes();
        record.extend_from_slice(path);
        record.push(0);
        record
    }

    #[test]
    fn parses_branch_metadata_and_independent_change_facets() {
        let mut fixture = format!(
            "# branch.oid {ONE_OID}\0# branch.head main\0# branch.upstream origin/main\0# branch.ab +3 -2\0"
        )
        .into_bytes();
        fixture.extend(ordinary("MM", ["100644", "100755", "100644"], b"both.txt"));
        fixture.extend(ordinary("A.", ["000000", "100644", "100644"], b"added.txt"));
        fixture.extend(ordinary(
            ".D",
            ["100644", "100644", "000000"],
            b"deleted.txt",
        ));
        fixture.extend(ordinary("T.", ["100644", "120000", "120000"], b"type.txt"));
        fixture.extend_from_slice(b"? -leading\nname.txt\0");
        fixture.extend(unmerged("UU", b"conflicted.txt"));

        let parsed = parse_status(&fixture).expect("valid detailed status");
        assert_eq!(parsed.head.branch.as_deref(), Some("main"));
        assert_eq!(parsed.head.upstream.as_deref(), Some("origin/main"));
        assert_eq!(parsed.head.ahead, Some(3));
        assert_eq!(parsed.head.behind, Some(2));
        assert_eq!(parsed.working_tree.staged, 3);
        assert_eq!(parsed.working_tree.unstaged, 2);
        assert_eq!(parsed.working_tree.untracked, 1);
        assert_eq!(parsed.working_tree.conflicted, 1);
        let both = parsed
            .changes
            .iter()
            .find(|change| change.path == b"both.txt")
            .expect("both-sided entry");
        assert_eq!(
            both.staged.as_ref().map(|facet| facet.kind),
            Some(ChangeKind::Modified)
        );
        assert_eq!(
            both.unstaged.as_ref().map(|facet| facet.kind),
            Some(ChangeKind::Modified)
        );
        assert_eq!(
            both.staged
                .as_ref()
                .and_then(|facet| facet.old_mode.as_deref()),
            Some("100644")
        );
        assert_eq!(
            both.staged
                .as_ref()
                .and_then(|facet| facet.new_mode.as_deref()),
            Some("100755")
        );
    }

    #[test]
    fn parses_rename_copy_origin_framing_and_similarity() {
        let mut fixture =
            format!("2 R. N... 100644 100644 100644 {ONE_OID} {TWO_OID} R075 renamed.txt\0")
                .into_bytes();
        fixture.extend_from_slice(b"origin.txt\0");
        fixture.extend_from_slice(
            format!("2 .C N... 100644 100644 100644 {ONE_OID} {TWO_OID} C100 copied.txt\0")
                .as_bytes(),
        );
        fixture.extend_from_slice(b"source.txt\0");

        let parsed = parse_status(&fixture).expect("valid movement records");
        let copied = &parsed.changes[0];
        let renamed = &parsed.changes[1];
        assert_eq!(copied.path, b"copied.txt");
        assert_eq!(
            copied.original_path.as_deref(),
            Some(b"source.txt".as_slice())
        );
        assert_eq!(
            copied.unstaged.as_ref().map(|facet| facet.kind),
            Some(ChangeKind::Copied)
        );
        assert_eq!(
            copied.unstaged.as_ref().and_then(|facet| facet.similarity),
            Some(100)
        );
        assert_eq!(renamed.path, b"renamed.txt");
        assert_eq!(
            renamed.original_path.as_deref(),
            Some(b"origin.txt".as_slice())
        );
        assert_eq!(
            renamed.staged.as_ref().map(|facet| facet.kind),
            Some(ChangeKind::Renamed)
        );
        assert_eq!(
            renamed.staged.as_ref().and_then(|facet| facet.similarity),
            Some(75)
        );
    }

    #[test]
    fn parses_every_conflict_kind_and_submodule_flags() {
        let cases = [
            ("DD", ConflictKind::BothDeleted),
            ("AU", ConflictKind::AddedByUs),
            ("UD", ConflictKind::DeletedByThem),
            ("UA", ConflictKind::AddedByThem),
            ("DU", ConflictKind::DeletedByUs),
            ("AA", ConflictKind::BothAdded),
            ("UU", ConflictKind::BothModified),
        ];
        for (xy, expected) in cases {
            let parsed = parse_status(&unmerged(xy, xy.as_bytes())).expect("valid conflict");
            assert_eq!(parsed.changes[0].conflict, Some(expected));
        }

        let record = format!("1 M. SCMU 160000 160000 160000 {ONE_OID} {TWO_OID} submodule\0");
        let parsed = parse_status(record.as_bytes()).expect("valid submodule state");
        assert_eq!(
            parsed.changes[0].submodule,
            Some(SubmoduleState {
                commit_changed: true,
                tracked_changes: true,
                untracked_changes: true,
            })
        );
    }

    #[test]
    fn preserves_unusual_and_invalid_utf8_path_bytes_in_byte_order() {
        let paths: [&[u8]; 5] = [
            b"unicode-\xe6\x96\x87\xe4\xbb\xb6",
            b"tab\tname",
            b"line\nname",
            b"invalid-\xff",
            b"-leading",
        ];
        let mut fixture = Vec::new();
        for path in paths {
            fixture.extend_from_slice(b"? ");
            fixture.extend_from_slice(path);
            fixture.push(0);
        }
        let parsed = parse_status(&fixture).expect("byte-safe paths");
        let actual = parsed
            .changes
            .iter()
            .map(|change| change.path.as_slice())
            .collect::<Vec<_>>();
        let mut expected = paths.to_vec();
        expected.sort();
        assert_eq!(actual, expected);
    }

    #[test]
    fn combines_compatible_records_for_one_byte_path() {
        let mut fixture = ordinary("D.", ["100644", "000000", "000000"], b"recreated");
        fixture.extend_from_slice(b"? recreated\0");
        let parsed = parse_status(&fixture).expect("compatible duplicate path");
        assert_eq!(parsed.changes.len(), 1);
        assert_eq!(
            parsed.changes[0].staged.as_ref().map(|facet| facet.kind),
            Some(ChangeKind::Deleted)
        );
        assert_eq!(
            parsed.changes[0].unstaged.as_ref().map(|facet| facet.kind),
            Some(ChangeKind::Untracked)
        );
        assert_eq!(parsed.working_tree.staged, 1);
        assert_eq!(parsed.working_tree.untracked, 1);
    }

    #[test]
    fn rejects_malformed_fixed_fields_framing_and_duplicates() {
        let malformed: [&[u8]; 6] = [
            b"1 M. N... 10064x 100644 100644 1111111111111111111111111111111111111111 2222222222222222222222222222222222222222 path\0",
            b"1 Z. N... 100644 100644 100644 1111111111111111111111111111111111111111 2222222222222222222222222222222222222222 path\0",
            b"2 R. N... 100644 100644 100644 1111111111111111111111111111111111111111 2222222222222222222222222222222222222222 R101 path\0origin\0",
            b"? missing-terminator",
            b"? ../escape\0",
            b"x unknown\0",
        ];
        for fixture in malformed {
            assert!(
                parse_status(fixture).is_err(),
                "fixture should fail: {fixture:?}"
            );
        }
        let incomplete_rename =
            format!("2 R. N... 100644 100644 100644 {ONE_OID} {TWO_OID} R100 path\0");
        assert!(parse_status(incomplete_rename.as_bytes()).is_err());
        assert!(parse_status(b"? same\0? same\0").is_err());
    }

    #[test]
    fn parses_filter_driver_names_case_insensitively_and_bounds_them() {
        let fixture = concat!(
            "filter.first.clean\0",
            "filter.first.required\0",
            "FILTER.second.PROCESS\0",
            "diff.third.command\0",
        );
        let drivers = parse_filter_drivers(fixture.as_bytes()).expect("valid filter names");
        assert_eq!(drivers.into_iter().collect::<Vec<_>>(), ["first", "second"]);

        let long = format!("filter.{}.clean\0", "a".repeat(MAX_FILTER_DRIVER_BYTES + 1));
        assert!(parse_filter_drivers(long.as_bytes()).is_err());
        assert!(parse_filter_drivers(b"filter.\xff.clean\0").is_err());
    }

    #[test]
    fn detailed_status_arguments_are_explicit_and_fail_closed() {
        let drivers = BTreeSet::from(["orbit".to_owned()]);
        let secure = status_arguments(&drivers, true);
        let secure = secure
            .iter()
            .map(|argument| argument.to_string_lossy())
            .collect::<Vec<_>>();
        for required in [
            "core.fsmonitor=false",
            "status.renames=copies",
            "diff.renameLimit=1000",
            "filter.orbit.clean=",
            "filter.orbit.process=",
            "filter.orbit.required=false",
            "--no-pager",
            "--no-lazy-fetch",
            "--no-optional-locks",
            "--porcelain=v2",
            "--branch",
            "--untracked-files=all",
            "--ignore-submodules=dirty",
            "--find-renames=50%",
            "-z",
        ] {
            assert!(
                secure.iter().any(|argument| argument == required),
                "missing {required}"
            );
        }
        let m0 = status_arguments(&BTreeSet::new(), false);
        assert!(!m0.iter().any(|argument| argument == "--no-lazy-fetch"));

        let error = read_detailed_status(&GitRunner::with_executable("false"), Path::new("."))
            .expect_err("secure capability is required");
        assert_eq!(error.code, "git_capability_unavailable");
    }

    #[test]
    fn accepts_unborn_and_detached_headers_but_rejects_invalid_utf8_metadata() {
        let unborn =
            parse_status(b"# branch.oid (initial)\0# branch.head main\0").expect("unborn status");
        assert_eq!(unborn.head.oid, None);
        assert_eq!(unborn.head.branch.as_deref(), Some("main"));
        let detached =
            parse_status(format!("# branch.oid {ONE_OID}\0# branch.head (detached)\0").as_bytes())
                .expect("detached status");
        assert!(detached.head.detached);
        assert_eq!(detached.head.branch, None);
        assert!(parse_status(b"# branch.head invalid-\xff\0").is_err());
    }

    #[test]
    fn enforces_entry_path_and_retained_path_bounds() {
        let oversized_path = vec![b'a'; MAX_CHANGE_PATH_BYTES + 1];
        let mut fixture = b"? ".to_vec();
        fixture.extend_from_slice(&oversized_path);
        fixture.push(0);
        assert_eq!(
            parse_status(&fixture).expect_err("oversized path").code,
            "git_command_failed"
        );

        let mut fixture = Vec::new();
        for index in 0..=MAX_CHANGE_ENTRIES {
            fixture.extend_from_slice(format!("? file-{index:05}\0").as_bytes());
        }
        assert_eq!(
            parse_status(&fixture).expect_err("too many entries").code,
            "git_command_failed"
        );

        let mut fixture = Vec::new();
        for index in 0..513 {
            let path_prefix = format!("{index:04}/");
            fixture.extend_from_slice(b"? ");
            fixture.extend_from_slice(path_prefix.as_bytes());
            fixture.extend(std::iter::repeat_n(
                b'a',
                MAX_CHANGE_PATH_BYTES - path_prefix.len(),
            ));
            fixture.push(0);
        }
        assert_eq!(
            parse_status(&fixture)
                .expect_err("retained path bytes above limit")
                .code,
            "git_command_failed"
        );
    }
}
