use std::{collections::BTreeSet, ffi::OsString, path::Path};

use serde::Serialize;

use crate::error::OrbitError;

use super::GitRunner;

const STATUS_OUTPUT_LIMIT: usize = 16 * 1024 * 1024;
const CONFIG_OUTPUT_LIMIT: usize = 1024 * 1024;
const MAX_FILTER_DRIVERS: usize = 256;
const MAX_FILTER_DRIVER_BYTES: usize = 1024;

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

pub struct StatusSnapshot {
    pub head: HeadSnapshot,
    pub working_tree: WorkingTreeSnapshot,
}

pub fn read_status(runner: &GitRunner, root: &Path) -> Result<StatusSnapshot, OrbitError> {
    let filter_drivers = configured_filter_drivers(runner, root)?;
    let mut args = Vec::with_capacity(10 + filter_drivers.len() * 6);
    args.extend([OsString::from("-c"), OsString::from("core.fsmonitor=false")]);
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
    args.extend(
        [
            "--no-optional-locks",
            "status",
            "--porcelain=v2",
            "--branch",
            "--untracked-files=normal",
            "-z",
        ]
        .map(OsString::from),
    );

    let output = runner.run(Some(root), "read_status", args, STATUS_OUTPUT_LIMIT)?;
    let output = runner.require_success("read_status", output)?;

    parse_status(&output.stdout)
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
    let mut head = HeadSnapshot {
        oid: None,
        branch: None,
        detached: false,
        upstream: None,
        ahead: None,
        behind: None,
    };
    let mut staged = 0;
    let mut unstaged = 0;
    let mut untracked = 0;
    let mut conflicted = 0;
    let records: Vec<&[u8]> = output.split(|byte| *byte == 0).collect();
    let mut index = 0;

    while index < records.len() {
        let record = records[index];
        index += 1;

        if record.is_empty() {
            continue;
        }

        match record[0] {
            b'#' => parse_header(record, &mut head)?,
            b'1' => count_changed_record(record, &mut staged, &mut unstaged, &mut conflicted)?,
            b'2' => {
                count_changed_record(record, &mut staged, &mut unstaged, &mut conflicted)?;
                if index >= records.len() || records[index].is_empty() {
                    return Err(malformed_status());
                }
                index += 1;
            }
            b'u' => conflicted += 1,
            b'?' => untracked += 1,
            b'!' => {}
            _ => {
                return Err(OrbitError::unsupported(
                    "parse_status",
                    "Git returned an unknown porcelain status record.",
                ));
            }
        }
    }

    let clean = staged == 0 && unstaged == 0 && untracked == 0 && conflicted == 0;

    Ok(StatusSnapshot {
        head,
        working_tree: WorkingTreeSnapshot {
            staged,
            unstaged,
            untracked,
            conflicted,
            clean,
        },
    })
}

fn parse_header(record: &[u8], head: &mut HeadSnapshot) -> Result<(), OrbitError> {
    const OID: &[u8] = b"# branch.oid ";
    const BRANCH: &[u8] = b"# branch.head ";
    const UPSTREAM: &[u8] = b"# branch.upstream ";
    const AHEAD_BEHIND: &[u8] = b"# branch.ab ";

    if let Some(value) = record.strip_prefix(OID) {
        if value != b"(initial)" {
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

fn count_changed_record(
    record: &[u8],
    staged: &mut u64,
    unstaged: &mut u64,
    conflicted: &mut u64,
) -> Result<(), OrbitError> {
    if record.len() < 4 || record[1] != b' ' {
        return Err(malformed_status());
    }

    let index_status = record[2];
    let work_tree_status = record[3];
    if index_status == b'U' || work_tree_status == b'U' {
        *conflicted += 1;
        return Ok(());
    }
    if index_status != b'.' {
        *staged += 1;
    }
    if work_tree_status != b'.' {
        *unstaged += 1;
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

    #[test]
    fn parses_branch_metadata_and_all_working_tree_counts() {
        let fixture = concat!(
            "# branch.oid 0123456789012345678901234567890123456789\0",
            "# branch.head main\0",
            "# branch.upstream origin/main\0",
            "# branch.ab +3 -2\0",
            "1 M. N... 100644 100644 100644 abc def staged file\0",
            "1 .M N... 100644 100644 100644 abc def unstaged\nfile\0",
            "1 MM N... 100644 100644 100644 abc def both.txt\0",
            "? -leading-name\0",
            "u UU N... 100644 100644 100644 100644 a b c conflicted.txt\0",
        );

        let parsed = parse_status(fixture.as_bytes()).expect("valid fixture");

        assert_eq!(parsed.head.branch.as_deref(), Some("main"));
        assert_eq!(parsed.head.upstream.as_deref(), Some("origin/main"));
        assert_eq!(parsed.head.ahead, Some(3));
        assert_eq!(parsed.head.behind, Some(2));
        assert_eq!(parsed.working_tree.staged, 2);
        assert_eq!(parsed.working_tree.unstaged, 2);
        assert_eq!(parsed.working_tree.untracked, 1);
        assert_eq!(parsed.working_tree.conflicted, 1);
        assert!(!parsed.working_tree.clean);
    }

    #[test]
    fn skips_the_second_path_in_a_rename_record() {
        let fixture = b"# branch.oid (initial)\x00# branch.head main\x002 R. N... 100644 100644 100644 abc def R100 new name\x00old\nname\x00";
        let parsed = parse_status(fixture).expect("valid rename fixture");

        assert_eq!(parsed.head.oid, None);
        assert_eq!(parsed.working_tree.staged, 1);
        assert_eq!(parsed.working_tree.untracked, 0);
    }

    #[test]
    fn parses_detached_head() {
        let fixture = b"# branch.oid abcdef\0# branch.head (detached)\0";
        let parsed = parse_status(fixture).expect("valid detached fixture");

        assert!(parsed.head.detached);
        assert_eq!(parsed.head.branch, None);
        assert!(parsed.working_tree.clean);
    }

    #[test]
    fn parses_only_executable_filter_driver_configuration() {
        let fixture = concat!(
            "filter.first.clean\0",
            "filter.first.required\0",
            "FILTER.second.PROCESS\0",
            "diff.third.command\0",
        );

        let drivers = parse_filter_drivers(fixture.as_bytes()).expect("valid filter names");

        assert_eq!(drivers.into_iter().collect::<Vec<_>>(), ["first", "second"]);
    }
}
