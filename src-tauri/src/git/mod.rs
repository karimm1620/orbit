mod diff;
mod graph;
mod history;
mod process;
mod status;

pub(crate) use diff::{read_file_diff, DiffSelection};
#[cfg(test)]
pub use diff::{DiffLineKind, DiffUnavailableReason, FileDiffContent};
pub use diff::{DiffSide, FileDiff};
#[cfg(test)]
pub use graph::CommitRefKind;
pub use graph::{
    describe_head, history_starting_tips, read_commit_refs, read_graph_commit_page,
    read_graph_order, read_object_format, CommitHistoryHead, CommitRef, GraphCommit, ObjectFormat,
};
pub use history::{read_recent_commits, CommitSummary};
pub use process::GitRunner;
#[cfg(test)]
pub use status::ChangeKind;
pub(crate) use status::{read_detailed_status, StatusEntry};
pub use status::{
    read_status, ChangeFacet, ConflictKind, HeadSnapshot, SubmoduleState, WorkingTreeSnapshot,
};
