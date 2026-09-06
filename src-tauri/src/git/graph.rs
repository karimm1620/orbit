use std::{collections::HashSet, ffi::OsString, path::Path};

use serde::Serialize;

use crate::error::OrbitError;

use super::{GitRunner, HeadSnapshot};

const HISTORY_OUTPUT_LIMIT: usize = 8 * 1024 * 1024;
const HISTORY_ORDER_OUTPUT_LIMIT: usize = 128 * 1024;
const REF_OUTPUT_LIMIT: usize = 4 * 1024 * 1024;
const OBJECT_FORMAT_OUTPUT_LIMIT: usize = 1024;
const HISTORY_FIELDS: usize = 7;
const REF_FIELDS: usize = 6;
const MINIMUM_SHORT_OID_LENGTH: usize = 12;
const MAX_REFS: usize = 4096;
const MINIMUM_SAFE_JAVASCRIPT_INTEGER: i64 = -9_007_199_254_740_991;
const MAXIMUM_SAFE_JAVASCRIPT_INTEGER: i64 = 9_007_199_254_740_991;
const HISTORY_FORMAT_ARGUMENT: &str = "--format=%H%x00%h%x00%P%x00%an%x00%at%x00%ct%x00%s";
const HISTORY_ORDER_FORMAT_ARGUMENT: &str = "--format=%H";
const REF_FORMAT_ARGUMENT: &str = "--format=%(objectname)%00%(objecttype)%00%(*objectname)%00%(*objecttype)%00%(refname)%00%(symref)";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphCommit {
    pub oid: String,
    pub short_oid: String,
    pub parent_oids: Vec<String>,
    pub subject: String,
    pub author_name: String,
    pub author_timestamp: i64,
    pub committed_timestamp: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CommitRefKind {
    LocalBranch,
    RemoteTrackingBranch,
    LightweightTag,
    AnnotatedTag,
    SymbolicRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitRef {
    pub kind: CommitRefKind,
    pub full_name: String,
    pub display_name: String,
    /// Always the commit OID. Annotated tag object OIDs are intentionally not exposed.
    pub target_oid: String,
    pub symbolic_target: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum CommitHistoryHead {
    Attached { branch: String, oid: String },
    Detached { oid: String },
    Unborn { branch: String },
}

impl CommitHistoryHead {
    pub fn oid(&self) -> Option<&str> {
        match self {
            Self::Attached { oid, .. } | Self::Detached { oid } => Some(oid),
            Self::Unborn { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectFormat {
    Sha1,
    Sha256,
}

impl ObjectFormat {
    fn oid_length(self) -> usize {
        match self {
            Self::Sha1 => 40,
            Self::Sha256 => 64,
        }
    }
}

pub fn read_object_format(runner: &GitRunner, root: &Path) -> Result<ObjectFormat, OrbitError> {
    runner.require_no_lazy_fetch()?;
    let output = runner.run(
        Some(root),
        "read_object_format",
        [
            "--no-pager",
            "--no-lazy-fetch",
            "--no-optional-locks",
            "rev-parse",
            "--show-object-format",
        ],
        OBJECT_FORMAT_OUTPUT_LIMIT,
    )?;
    let output = runner.require_success("read_object_format", output)?;

    match output.stdout.as_slice() {
        b"sha1\n" => Ok(ObjectFormat::Sha1),
        b"sha256\n" => Ok(ObjectFormat::Sha256),
        _ => Err(OrbitError::unsupported(
            "read_object_format",
            "Git returned an unsupported repository object format.",
        )),
    }
}

pub fn describe_head(
    head: &HeadSnapshot,
    object_format: ObjectFormat,
) -> Result<CommitHistoryHead, OrbitError> {
    match (&head.branch, &head.oid, head.detached) {
        (Some(branch), Some(oid), false) => {
            validate_oid(oid, object_format, "parse_commit_history")?;
            Ok(CommitHistoryHead::Attached {
                branch: branch.clone(),
                oid: oid.clone(),
            })
        }
        (None, Some(oid), true) => {
            validate_oid(oid, object_format, "parse_commit_history")?;
            Ok(CommitHistoryHead::Detached { oid: oid.clone() })
        }
        (Some(branch), None, false) => Ok(CommitHistoryHead::Unborn {
            branch: branch.clone(),
        }),
        _ => Err(OrbitError::unsupported(
            "read_commit_history",
            "Git returned an inconsistent HEAD state.",
        )),
    }
}

pub fn read_commit_refs(
    runner: &GitRunner,
    root: &Path,
    object_format: ObjectFormat,
) -> Result<Vec<CommitRef>, OrbitError> {
    runner.require_no_lazy_fetch()?;
    let output = runner.run(
        Some(root),
        "read_commit_refs",
        [
            "--no-pager",
            "--no-lazy-fetch",
            "--no-optional-locks",
            "for-each-ref",
            "--sort=refname",
            REF_FORMAT_ARGUMENT,
            "refs/heads",
            "refs/remotes",
            "refs/tags",
        ],
        REF_OUTPUT_LIMIT,
    )?;
    let output = runner.require_success("read_commit_refs", output)?;

    parse_commit_refs(&output.stdout, object_format)
}

pub fn read_graph_order(
    runner: &GitRunner,
    root: &Path,
    starting_tips: &[String],
    limit: usize,
    object_format: ObjectFormat,
) -> Result<Vec<String>, OrbitError> {
    if starting_tips.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    for oid in starting_tips {
        validate_oid(oid, object_format, "read_commit_history")?;
    }

    runner.require_no_lazy_fetch()?;
    let arguments = graph_order_arguments(starting_tips, limit);

    let output = runner.run(
        Some(root),
        "plan_commit_history",
        arguments,
        HISTORY_ORDER_OUTPUT_LIMIT,
    )?;
    let output = runner.require_success("plan_commit_history", output)?;

    parse_graph_order(&output.stdout, object_format, limit)
}

pub fn read_graph_commit_page(
    runner: &GitRunner,
    root: &Path,
    ordered_oids: &[String],
    object_format: ObjectFormat,
) -> Result<Vec<GraphCommit>, OrbitError> {
    if ordered_oids.is_empty() {
        return Ok(Vec::new());
    }
    for oid in ordered_oids {
        validate_oid(oid, object_format, "read_commit_history")?;
    }

    runner.require_no_lazy_fetch()?;
    let arguments = graph_page_arguments(ordered_oids);
    let output = runner.run(
        Some(root),
        "read_commit_history",
        arguments,
        HISTORY_OUTPUT_LIMIT,
    )?;
    let output = runner.require_success("read_commit_history", output)?;
    let commits = parse_graph_history(&output.stdout, object_format, ordered_oids.len())?;
    if commits.len() != ordered_oids.len()
        || commits
            .iter()
            .zip(ordered_oids)
            .any(|(commit, expected_oid)| commit.oid != *expected_oid)
    {
        return Err(OrbitError::unsupported(
            "read_commit_history",
            "Git did not return the requested commit-history page in session order.",
        ));
    }

    Ok(commits)
}

fn graph_order_arguments(starting_tips: &[String], limit: usize) -> Vec<OsString> {
    let mut arguments = hardened_log_arguments(HISTORY_ORDER_FORMAT_ARGUMENT);
    arguments.push(OsString::from("--topo-order"));
    arguments.push(OsString::from(format!("--max-count={limit}")));
    arguments.extend(starting_tips.iter().map(OsString::from));
    arguments.push(OsString::from("--"));
    arguments
}

fn graph_page_arguments(ordered_oids: &[String]) -> Vec<OsString> {
    let mut arguments = hardened_log_arguments(HISTORY_FORMAT_ARGUMENT);
    arguments.push(OsString::from("--no-walk=unsorted"));
    arguments.extend(ordered_oids.iter().map(OsString::from));
    arguments.push(OsString::from("--"));
    arguments
}

fn hardened_log_arguments(format: &str) -> Vec<OsString> {
    let mut arguments = Vec::with_capacity(19);
    arguments.extend(
        [
            "--no-pager",
            "--no-lazy-fetch",
            "--no-optional-locks",
            "-c",
            "core.abbrev=12",
            "-c",
            "log.showSignature=false",
            "log",
            "--no-decorate",
            "--no-notes",
            "--no-patch",
            "--no-ext-diff",
            "--no-textconv",
            "--encoding=UTF-8",
        ]
        .map(OsString::from),
    );
    arguments.push(OsString::from(format));
    arguments.push(OsString::from("-z"));
    arguments
}

pub fn history_starting_tips(head: &CommitHistoryHead, refs: &[CommitRef]) -> Vec<String> {
    let mut seen = HashSet::new();
    head.oid()
        .into_iter()
        .chain(refs.iter().map(|commit_ref| commit_ref.target_oid.as_str()))
        .filter(|oid| seen.insert((*oid).to_owned()))
        .map(ToOwned::to_owned)
        .collect()
}

fn parse_graph_history(
    output: &[u8],
    object_format: ObjectFormat,
    maximum_commits: usize,
) -> Result<Vec<GraphCommit>, OrbitError> {
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
    if fields.len() / HISTORY_FIELDS > maximum_commits {
        return Err(OrbitError::output_too_large("parse_commit_history"));
    }

    fields
        .chunks(HISTORY_FIELDS)
        .map(|fields| {
            let oid = history_text(fields[0], "object ID")?;
            validate_oid(oid, object_format, "parse_commit_history")?;
            let short_oid = history_text(fields[1], "short object ID")?;
            if short_oid.len() < MINIMUM_SHORT_OID_LENGTH
                || short_oid.len() > oid.len()
                || !short_oid.bytes().all(|byte| byte.is_ascii_hexdigit())
                || !oid.starts_with(short_oid)
            {
                return Err(malformed_history());
            }

            let parents = history_text(fields[2], "parent object IDs")?;
            let parent_oids = if parents.is_empty() {
                Vec::new()
            } else {
                parents
                    .split(' ')
                    .map(|parent| {
                        validate_oid(parent, object_format, "parse_commit_history")?;
                        Ok(parent.to_owned())
                    })
                    .collect::<Result<Vec<_>, OrbitError>>()?
            };

            Ok(GraphCommit {
                oid: oid.to_owned(),
                short_oid: short_oid.to_owned(),
                parent_oids,
                author_name: history_text(fields[3], "author name")?.to_owned(),
                author_timestamp: parse_timestamp(fields[4])?,
                committed_timestamp: parse_timestamp(fields[5])?,
                subject: history_text(fields[6], "subject")?.to_owned(),
            })
        })
        .collect()
}

fn parse_graph_order(
    output: &[u8],
    object_format: ObjectFormat,
    maximum_commits: usize,
) -> Result<Vec<String>, OrbitError> {
    if output.is_empty() {
        return Ok(Vec::new());
    }

    let mut fields: Vec<&[u8]> = output.split(|byte| *byte == 0).collect();
    if fields.last() == Some(&&[][..]) {
        fields.pop();
    }
    if fields.len() > maximum_commits {
        return Err(OrbitError::output_too_large("parse_commit_history_order"));
    }

    let mut seen = HashSet::with_capacity(fields.len());
    fields
        .into_iter()
        .map(|field| {
            let oid = history_order_text(field)?;
            validate_oid(oid, object_format, "parse_commit_history_order")?;
            if !seen.insert(oid.to_owned()) {
                return Err(OrbitError::unsupported(
                    "parse_commit_history_order",
                    "Git returned a duplicate commit in the history order.",
                ));
            }
            Ok(oid.to_owned())
        })
        .collect()
}

fn history_order_text(value: &[u8]) -> Result<&str, OrbitError> {
    std::str::from_utf8(value).map_err(|_| {
        OrbitError::unsupported(
            "parse_commit_history_order",
            "A commit object ID is not valid UTF-8.",
        )
    })
}

fn parse_commit_refs(
    output: &[u8],
    object_format: ObjectFormat,
) -> Result<Vec<CommitRef>, OrbitError> {
    if output.is_empty() {
        return Ok(Vec::new());
    }

    let mut refs = Vec::new();
    let mut records = output.split(|byte| *byte == b'\n').peekable();
    while let Some(record) = records.next() {
        if record.is_empty() && records.peek().is_none() {
            break;
        }
        if record.is_empty() {
            return Err(malformed_refs());
        }

        let fields: Vec<&[u8]> = record.split(|byte| *byte == 0).collect();
        if fields.len() != REF_FIELDS {
            return Err(malformed_refs());
        }
        if let Some(commit_ref) = parse_ref_record(&fields, object_format)? {
            refs.push(commit_ref);
            if refs.len() > MAX_REFS {
                return Err(OrbitError::output_too_large("parse_commit_refs"));
            }
        }
    }

    Ok(refs)
}

fn parse_ref_record(
    fields: &[&[u8]],
    object_format: ObjectFormat,
) -> Result<Option<CommitRef>, OrbitError> {
    let object_oid = ref_text(fields[0], "object ID")?;
    let object_type = ref_text(fields[1], "object type")?;
    let peeled_oid = ref_text(fields[2], "peeled object ID")?;
    let peeled_type = ref_text(fields[3], "peeled object type")?;
    let full_name = ref_text(fields[4], "full name")?;
    let symbolic_target = ref_text(fields[5], "symbolic target")?;

    let (namespace, display_name) = ref_namespace(full_name).ok_or_else(malformed_refs)?;
    let symbolic_target = (!symbolic_target.is_empty()).then(|| symbolic_target.to_owned());

    let (kind, target_oid) = if symbolic_target.is_some() {
        if object_type != "commit" || !peeled_oid.is_empty() || !peeled_type.is_empty() {
            return Err(malformed_refs());
        }
        validate_oid(object_oid, object_format, "parse_commit_refs")?;
        (CommitRefKind::SymbolicRef, object_oid)
    } else if object_type == "commit" {
        if !peeled_oid.is_empty() || !peeled_type.is_empty() {
            return Err(malformed_refs());
        }
        validate_oid(object_oid, object_format, "parse_commit_refs")?;
        let kind = match namespace {
            RefNamespace::Local => CommitRefKind::LocalBranch,
            RefNamespace::Remote => CommitRefKind::RemoteTrackingBranch,
            RefNamespace::Tag => CommitRefKind::LightweightTag,
        };
        (kind, object_oid)
    } else if object_type == "tag" && namespace == RefNamespace::Tag {
        validate_oid(object_oid, object_format, "parse_commit_refs")?;
        if peeled_type != "commit" {
            return Ok(None);
        }
        validate_oid(peeled_oid, object_format, "parse_commit_refs")?;
        (CommitRefKind::AnnotatedTag, peeled_oid)
    } else {
        return Ok(None);
    };

    Ok(Some(CommitRef {
        kind,
        full_name: full_name.to_owned(),
        display_name: display_name.to_owned(),
        target_oid: target_oid.to_owned(),
        symbolic_target,
    }))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RefNamespace {
    Local,
    Remote,
    Tag,
}

fn ref_namespace(full_name: &str) -> Option<(RefNamespace, &str)> {
    if let Some(display_name) = full_name.strip_prefix("refs/heads/") {
        Some((RefNamespace::Local, display_name))
    } else if let Some(display_name) = full_name.strip_prefix("refs/remotes/") {
        Some((RefNamespace::Remote, display_name))
    } else {
        full_name
            .strip_prefix("refs/tags/")
            .map(|display_name| (RefNamespace::Tag, display_name))
    }
}

fn validate_oid(
    value: &str,
    object_format: ObjectFormat,
    operation: &'static str,
) -> Result<(), OrbitError> {
    if value.len() == object_format.oid_length()
        && value.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        Ok(())
    } else {
        Err(OrbitError::unsupported(
            operation,
            "Git returned a malformed object ID.",
        ))
    }
}

fn parse_timestamp(value: &[u8]) -> Result<i64, OrbitError> {
    let timestamp = history_text(value, "timestamp")?
        .parse()
        .map_err(|_| malformed_history())?;
    if !(MINIMUM_SAFE_JAVASCRIPT_INTEGER..=MAXIMUM_SAFE_JAVASCRIPT_INTEGER).contains(&timestamp) {
        return Err(malformed_history());
    }
    Ok(timestamp)
}

fn history_text<'a>(value: &'a [u8], field: &str) -> Result<&'a str, OrbitError> {
    std::str::from_utf8(value).map_err(|_| {
        OrbitError::unsupported(
            "parse_commit_history",
            format!("A commit {field} is not valid UTF-8."),
        )
    })
}

fn ref_text<'a>(value: &'a [u8], field: &str) -> Result<&'a str, OrbitError> {
    std::str::from_utf8(value).map_err(|_| {
        OrbitError::unsupported(
            "parse_commit_refs",
            format!("A Git ref {field} is not valid UTF-8."),
        )
    })
}

fn malformed_history() -> OrbitError {
    OrbitError::unsupported(
        "parse_commit_history",
        "Git returned malformed commit-history data.",
    )
}

fn malformed_refs() -> OrbitError {
    OrbitError::unsupported("parse_commit_refs", "Git returned malformed ref data.")
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const C: &str = "cccccccccccccccccccccccccccccccccccccccc";
    const D: &str = "dddddddddddddddddddddddddddddddddddddddd";

    fn history_record(oid: &str, parents: &str, author: &str, subject: &str) -> String {
        format!(
            "{oid}\0{}\0{parents}\0{author}\01700000000\01600000000\0{subject}\0",
            &oid[..12]
        )
    }

    #[test]
    fn parses_linear_merge_octopus_unicode_and_empty_subjects() {
        let fixture = [
            history_record(A, B, "Zoë", "Unicode ✓"),
            history_record(B, &format!("{C} {D}"), "Ada", "Merge"),
            history_record(C, &format!("{A} {B} {D}"), "Grace", "Octopus"),
            history_record(D, "", "Linus", ""),
        ]
        .concat();

        let commits =
            parse_graph_history(fixture.as_bytes(), ObjectFormat::Sha1, 4).expect("valid history");

        assert_eq!(commits.len(), 4);
        assert_eq!(commits[0].author_name, "Zoë");
        assert_eq!(commits[1].parent_oids, [C, D]);
        assert_eq!(commits[2].parent_oids, [A, B, D]);
        assert_eq!(commits[3].subject, "");
    }

    #[test]
    fn rejects_incomplete_and_invalid_utf8_history() {
        let incomplete =
            parse_graph_history(b"abc\0def\0", ObjectFormat::Sha1, 1).expect_err("partial record");
        assert_eq!(incomplete.code, "unsupported_repository_state");

        let mut invalid = history_record(A, "", "Author", "Subject").into_bytes();
        let subject = invalid.len() - "Subject\0".len();
        invalid[subject] = 0xff;
        let invalid =
            parse_graph_history(&invalid, ObjectFormat::Sha1, 1).expect_err("invalid UTF-8");
        assert_eq!(invalid.operation, "parse_commit_history");
    }

    #[test]
    fn rejects_more_records_than_the_call_bound() {
        let fixture = history_record(A, "", "Author", "Subject").repeat(2);
        let error = parse_graph_history(fixture.as_bytes(), ObjectFormat::Sha1, 1)
            .expect_err("too many commits");

        assert_eq!(error.code, "git_command_failed");
        assert_eq!(error.operation, "parse_commit_history");
    }

    #[test]
    fn rejects_timestamps_that_are_not_frontend_safe_integers() {
        let fixture = format!(
            "{A}\0{}\0\0Author\09007199254740992\01600000000\0Subject\0",
            &A[..12]
        );
        let error = parse_graph_history(fixture.as_bytes(), ObjectFormat::Sha1, 1)
            .expect_err("unsafe timestamp");

        assert_eq!(error.operation, "parse_commit_history");
    }

    #[test]
    fn parses_and_bounds_a_nul_framed_commit_order() {
        let fixture = format!("{A}\0{B}\0{C}\0");
        let order = parse_graph_order(fixture.as_bytes(), ObjectFormat::Sha1, 3)
            .expect("valid commit order");
        assert_eq!(order, [A, B, C]);

        let malformed =
            parse_graph_order(b"not-an-oid\0", ObjectFormat::Sha1, 1).expect_err("malformed order");
        assert_eq!(malformed.code, "unsupported_repository_state");

        let invalid_utf8 =
            parse_graph_order(&[0xff, 0], ObjectFormat::Sha1, 1).expect_err("invalid UTF-8 order");
        assert_eq!(invalid_utf8.operation, "parse_commit_history_order");

        let oversized_fixture = format!("{A}\0{B}\0");
        let oversized = parse_graph_order(oversized_fixture.as_bytes(), ObjectFormat::Sha1, 1)
            .expect_err("oversized order");
        assert_eq!(oversized.operation, "parse_commit_history_order");
    }

    #[test]
    fn parses_all_ref_kinds_and_unicode_names() {
        let fixture = format!(
            "{A}\0commit\0\0\0refs/heads/main\0\n\
             {B}\0commit\0\0\0refs/heads/naïve/topic\0\n\
             {A}\0commit\0\0\0refs/remotes/origin/main\0\n\
             {A}\0commit\0\0\0refs/remotes/origin/HEAD\0refs/remotes/origin/main\n\
             {C}\0commit\0\0\0refs/tags/lightweight\0\n\
             {D}\0tag\0{B}\0commit\0refs/tags/annotated/β\0\n"
        );

        let refs = parse_commit_refs(fixture.as_bytes(), ObjectFormat::Sha1).expect("valid refs");

        assert_eq!(refs.len(), 6);
        assert_eq!(refs[0].kind, CommitRefKind::LocalBranch);
        assert_eq!(refs[1].display_name, "naïve/topic");
        assert_eq!(refs[2].kind, CommitRefKind::RemoteTrackingBranch);
        assert_eq!(refs[3].kind, CommitRefKind::SymbolicRef);
        assert_eq!(refs[4].kind, CommitRefKind::LightweightTag);
        assert_eq!(refs[5].kind, CommitRefKind::AnnotatedTag);
        assert_eq!(refs[5].target_oid, B);
    }

    #[test]
    fn rejects_malformed_ref_framing() {
        let error = parse_commit_refs(b"missing\0fields\n", ObjectFormat::Sha1)
            .expect_err("malformed refs");

        assert_eq!(error.operation, "parse_commit_refs");
    }

    #[test]
    fn history_arguments_keep_execution_surfaces_disabled() {
        let order = graph_order_arguments(&[A.to_owned()], 1);
        let page = graph_page_arguments(&[A.to_owned()]);
        assert!(order.iter().any(|argument| argument == "--topo-order"));
        assert!(page.iter().any(|argument| argument == "--no-walk=unsorted"));
        let commands = [order, page];

        for command in commands {
            let arguments = command
                .into_iter()
                .map(|argument| argument.into_string().expect("UTF-8 argument"))
                .collect::<Vec<_>>();
            for argument in [
                "--no-pager",
                "--no-lazy-fetch",
                "--no-optional-locks",
                "log.showSignature=false",
                "--no-decorate",
                "--no-notes",
                "--no-patch",
                "--no-ext-diff",
                "--no-textconv",
            ] {
                assert!(arguments.iter().any(|candidate| candidate == argument));
            }
            assert_eq!(arguments.last().map(String::as_str), Some("--"));
        }
    }
}
