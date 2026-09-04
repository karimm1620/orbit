use std::path::Path;

use serde::Serialize;

use crate::error::OrbitError;

use super::GitRunner;

pub const RECENT_COMMIT_LIMIT: usize = 50;
const HISTORY_OUTPUT_LIMIT: usize = 4 * 1024 * 1024;
const HISTORY_FIELDS: usize = 6;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitSummary {
    pub oid: String,
    pub short_oid: String,
    pub parents: Vec<String>,
    pub subject: String,
    pub author_name: String,
    pub timestamp: i64,
}

pub fn read_recent_commits(
    runner: &GitRunner,
    root: &Path,
    has_head: bool,
) -> Result<Vec<CommitSummary>, OrbitError> {
    if !has_head {
        return Ok(Vec::new());
    }

    let limit = RECENT_COMMIT_LIMIT.to_string();
    let output = runner.run(
        Some(root),
        "read_history",
        [
            "--no-pager",
            "log",
            "-n",
            &limit,
            "--date-order",
            "--encoding=UTF-8",
            "--format=%H%x00%h%x00%P%x00%s%x00%an%x00%ct",
            "-z",
            "HEAD",
            "--",
        ],
        HISTORY_OUTPUT_LIMIT,
    )?;
    let output = runner.require_success("read_history", output)?;

    parse_history(&output.stdout)
}

fn parse_history(output: &[u8]) -> Result<Vec<CommitSummary>, OrbitError> {
    if output.is_empty() {
        return Ok(Vec::new());
    }

    let mut fields: Vec<&[u8]> = output.split(|byte| *byte == 0).collect();
    if fields.last() == Some(&&[][..]) {
        fields.pop();
    }
    if fields.len().checked_rem(HISTORY_FIELDS) != Some(0) {
        return Err(malformed_history());
    }

    fields
        .chunks(HISTORY_FIELDS)
        .map(|fields| {
            let oid = text(fields[0], "object ID")?;
            let short_oid = text(fields[1], "short object ID")?;
            if oid.is_empty()
                || short_oid.is_empty()
                || !oid.bytes().all(|byte| byte.is_ascii_hexdigit())
                || !short_oid.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                return Err(malformed_history());
            }

            let parents = text(fields[2], "parent object IDs")?
                .split_whitespace()
                .map(ToOwned::to_owned)
                .collect();
            let timestamp = text(fields[5], "timestamp")?
                .parse()
                .map_err(|_| malformed_history())?;

            Ok(CommitSummary {
                oid: oid.to_owned(),
                short_oid: short_oid.to_owned(),
                parents,
                subject: text(fields[3], "subject")?.to_owned(),
                author_name: text(fields[4], "author name")?.to_owned(),
                timestamp,
            })
        })
        .collect()
}

fn text<'a>(value: &'a [u8], field: &str) -> Result<&'a str, OrbitError> {
    std::str::from_utf8(value).map_err(|_| {
        OrbitError::unsupported(
            "parse_history",
            format!("A recent commit {field} is not valid UTF-8."),
        )
    })
}

fn malformed_history() -> OrbitError {
    OrbitError::unsupported(
        "parse_history",
        "Git returned malformed recent-history data.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_root_and_merge_commits() {
        let fixture = concat!(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\x00aaaaaaaa\x00",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb cccccccccccccccccccccccccccccccccccccccc\x00",
            "Merge naïve [topic] — ✓\x00Ada Lovelace\x001700000000\x00",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\x00bbbbbbbb\x00\x00",
            "Initial\x00Grace Hopper\x001600000000\x00",
        );

        let commits = parse_history(fixture.as_bytes()).expect("valid history");

        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].parents.len(), 2);
        assert_eq!(commits[1].parents.len(), 0);
        assert_eq!(commits[0].subject, "Merge naïve [topic] — ✓");
        assert_eq!(commits[0].timestamp, 1_700_000_000);
    }

    #[test]
    fn rejects_partial_records() {
        let error = parse_history(b"abc\0def\0").expect_err("partial record");
        assert_eq!(error.code, "unsupported_repository_state");
    }
}
