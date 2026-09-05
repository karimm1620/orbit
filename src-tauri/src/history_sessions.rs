use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};

use serde::Serialize;

use crate::{
    error::OrbitError,
    git::{
        describe_head, history_starting_tips, read_commit_refs, read_graph_commits,
        read_object_format, CommitHistoryHead, CommitRef, GitRunner, GraphCommit, HeadSnapshot,
        ObjectFormat,
    },
};

pub const DEFAULT_HISTORY_PAGE_SIZE: usize = 100;
pub const MAX_HISTORY_PAGE_SIZE: usize = 200;
pub const MAX_HISTORY_FRONTIER_TIPS: usize = 512;
pub const MAX_HISTORY_SESSION_COMMITS: usize = 1000;
pub const MAX_ACTIVE_HISTORY_SESSIONS: usize = 8;
const HISTORY_SESSION_IDLE_TTL: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct HistoryCursor(String);

impl HistoryCursor {
    #[cfg(test)]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitHistoryPage {
    pub commits: Vec<GraphCommit>,
    pub refs: Vec<CommitRef>,
    pub head: CommitHistoryHead,
    pub next_cursor: Option<HistoryCursor>,
    pub has_more: bool,
    pub session_limit_reached: bool,
}

struct CommitHistorySession {
    repository_id: String,
    root: PathBuf,
    head: CommitHistoryHead,
    object_format: ObjectFormat,
    frontier: Vec<String>,
    emitted: HashSet<String>,
    total_emitted: usize,
    last_access: Instant,
}

pub struct CommitHistoryRegistry {
    sessions: Mutex<HashMap<String, CommitHistorySession>>,
    next_cursor: AtomicU64,
}

impl Default for CommitHistoryRegistry {
    fn default() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            next_cursor: AtomicU64::new(1),
        }
    }
}

impl CommitHistoryRegistry {
    pub fn invalidate_repository(&self, repository_id: &str) -> Result<(), OrbitError> {
        let mut sessions = self.sessions.lock().map_err(|_| {
            OrbitError::internal(
                "read_commit_history",
                "Commit-history session state is unavailable.",
            )
        })?;
        sessions.retain(|_, session| session.repository_id != repository_id);
        Ok(())
    }

    pub fn start(
        &self,
        runner: &GitRunner,
        repository_id: &str,
        root: &Path,
        head_snapshot: &HeadSnapshot,
        requested_page_size: Option<i64>,
    ) -> Result<CommitHistoryPage, OrbitError> {
        let page_size = validate_page_size(requested_page_size)?;
        runner.require_no_lazy_fetch()?;
        let object_format = read_object_format(runner, root)?;
        let head = describe_head(head_snapshot, object_format)?;
        let refs = read_commit_refs(runner, root, object_format)?;
        let frontier = history_starting_tips(&head, &refs);
        ensure_frontier_bound(&frontier)?;

        if frontier.is_empty() {
            return Ok(CommitHistoryPage {
                commits: Vec::new(),
                refs,
                head,
                next_cursor: None,
                has_more: false,
                session_limit_reached: false,
            });
        }

        let commits = read_graph_commits(runner, root, &frontier, page_size, object_format)?;
        let mut session = CommitHistorySession {
            repository_id: repository_id.to_owned(),
            root: root.to_owned(),
            head: head.clone(),
            object_format,
            frontier,
            emitted: HashSet::new(),
            total_emitted: 0,
            last_access: Instant::now(),
        };
        let commits = advance_session(&mut session, commits)?;
        let (next_cursor, has_more, session_limit_reached) = self.finish_page(session)?;

        Ok(CommitHistoryPage {
            commits,
            refs,
            head,
            next_cursor,
            has_more,
            session_limit_reached,
        })
    }

    pub fn load_more(
        &self,
        runner: &GitRunner,
        repository_id: &str,
        root: &Path,
        cursor: &str,
        requested_page_size: Option<i64>,
    ) -> Result<CommitHistoryPage, OrbitError> {
        let page_size = validate_page_size(requested_page_size)?;
        let mut session = self.take_session(repository_id, root, cursor)?;
        runner.require_no_lazy_fetch()?;

        let remaining = MAX_HISTORY_SESSION_COMMITS.saturating_sub(session.total_emitted);
        if remaining == 0 {
            return Err(OrbitError::history_session_unavailable());
        }
        let effective_page_size = page_size.min(remaining);
        let commits = read_graph_commits(
            runner,
            root,
            &session.frontier,
            effective_page_size,
            session.object_format,
        )?;
        let commits = advance_session(&mut session, commits)?;
        let head = session.head.clone();
        let (next_cursor, has_more, session_limit_reached) = self.finish_page(session)?;

        Ok(CommitHistoryPage {
            commits,
            refs: Vec::new(),
            head,
            next_cursor,
            has_more,
            session_limit_reached,
        })
    }

    fn finish_page(
        &self,
        mut session: CommitHistorySession,
    ) -> Result<(Option<HistoryCursor>, bool, bool), OrbitError> {
        let has_more = !session.frontier.is_empty();
        let session_limit_reached =
            has_more && session.total_emitted >= MAX_HISTORY_SESSION_COMMITS;
        let next_cursor = if has_more && !session_limit_reached {
            session.last_access = Instant::now();
            Some(self.insert_session(session)?)
        } else {
            None
        };

        Ok((next_cursor, has_more, session_limit_reached))
    }

    fn insert_session(&self, session: CommitHistorySession) -> Result<HistoryCursor, OrbitError> {
        let cursor = self.new_cursor();
        let mut sessions = self.sessions.lock().map_err(|_| {
            OrbitError::internal(
                "read_commit_history",
                "Commit-history session state is unavailable.",
            )
        })?;
        prune_expired(&mut sessions, Instant::now());

        if sessions.len() >= MAX_ACTIVE_HISTORY_SESSIONS {
            let oldest = sessions
                .iter()
                .min_by(|(left_cursor, left), (right_cursor, right)| {
                    left.last_access
                        .cmp(&right.last_access)
                        .then_with(|| left_cursor.cmp(right_cursor))
                })
                .map(|(cursor, _)| cursor.clone());
            if let Some(oldest) = oldest {
                sessions.remove(&oldest);
            }
        }

        sessions.insert(cursor.0.clone(), session);
        Ok(cursor)
    }

    fn take_session(
        &self,
        repository_id: &str,
        root: &Path,
        cursor: &str,
    ) -> Result<CommitHistorySession, OrbitError> {
        if !valid_history_cursor(cursor) {
            return Err(OrbitError::history_session_unavailable());
        }

        let mut sessions = self.sessions.lock().map_err(|_| {
            OrbitError::internal(
                "read_commit_history",
                "Commit-history session state is unavailable.",
            )
        })?;
        prune_expired(&mut sessions, Instant::now());
        let session = sessions
            .get(cursor)
            .ok_or_else(OrbitError::history_session_unavailable)?;
        if session.repository_id != repository_id || session.root != root {
            return Err(OrbitError::history_session_unavailable());
        }

        sessions
            .remove(cursor)
            .ok_or_else(OrbitError::history_session_unavailable)
    }

    fn new_cursor(&self) -> HistoryCursor {
        let value = self.next_cursor.fetch_add(1, Ordering::Relaxed);
        HistoryCursor(format!("history-{value:016x}"))
    }

    #[cfg(test)]
    pub fn expire_for_test(&self, cursor: &str) {
        let mut sessions = self.sessions.lock().expect("history session lock");
        if let Some(session) = sessions.get_mut(cursor) {
            session.last_access = Instant::now()
                .checked_sub(HISTORY_SESSION_IDLE_TTL + Duration::from_secs(1))
                .expect("test instant supports subtraction");
        }
    }
}

fn validate_page_size(requested: Option<i64>) -> Result<usize, OrbitError> {
    let requested = requested.unwrap_or(DEFAULT_HISTORY_PAGE_SIZE as i64);
    if !(1..=MAX_HISTORY_PAGE_SIZE as i64).contains(&requested) {
        return Err(OrbitError::invalid_history_request(format!(
            "Choose a page size between 1 and {MAX_HISTORY_PAGE_SIZE} commits."
        )));
    }

    Ok(requested as usize)
}

fn advance_session(
    session: &mut CommitHistorySession,
    commits: Vec<GraphCommit>,
) -> Result<Vec<GraphCommit>, OrbitError> {
    let mut new_commits = Vec::with_capacity(commits.len());
    for commit in commits {
        if session.emitted.insert(commit.oid.clone()) {
            new_commits.push(commit);
        }
    }
    if new_commits.is_empty() && !session.frontier.is_empty() {
        return Err(OrbitError::unsupported(
            "read_commit_history",
            "Commit-history pagination did not advance.",
        ));
    }

    let mut next_frontier = Vec::new();
    let mut frontier_seen = HashSet::new();
    for oid in session.frontier.iter().chain(
        new_commits
            .iter()
            .flat_map(|commit| commit.parent_oids.iter()),
    ) {
        if !session.emitted.contains(oid) && frontier_seen.insert(oid.clone()) {
            next_frontier.push(oid.clone());
        }
    }
    ensure_frontier_bound(&next_frontier)?;

    session.frontier = next_frontier;
    session.total_emitted += new_commits.len();
    Ok(new_commits)
}

fn ensure_frontier_bound(frontier: &[String]) -> Result<(), OrbitError> {
    if frontier.len() > MAX_HISTORY_FRONTIER_TIPS {
        Err(OrbitError::unsupported(
            "read_commit_history",
            format!(
                "This repository has more than {MAX_HISTORY_FRONTIER_TIPS} active history tips."
            ),
        ))
    } else {
        Ok(())
    }
}

fn valid_history_cursor(cursor: &str) -> bool {
    cursor.strip_prefix("history-").is_some_and(|suffix| {
        suffix.len() == 16 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

fn prune_expired(sessions: &mut HashMap<String, CommitHistorySession>, now: Instant) {
    sessions.retain(|_, session| {
        now.checked_duration_since(session.last_access)
            .is_some_and(|idle| idle < HISTORY_SESSION_IDLE_TTL)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_session(total_emitted: usize) -> CommitHistorySession {
        CommitHistorySession {
            repository_id: "repository-0000000000000001".to_owned(),
            root: PathBuf::from("/test/repository"),
            head: CommitHistoryHead::Detached {
                oid: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
            },
            object_format: ObjectFormat::Sha1,
            frontier: vec!["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned()],
            emitted: HashSet::new(),
            total_emitted,
            last_access: Instant::now(),
        }
    }

    #[test]
    fn validates_default_and_bounded_page_sizes() {
        assert_eq!(validate_page_size(None).expect("default"), 100);
        assert_eq!(validate_page_size(Some(1)).expect("minimum"), 1);
        assert_eq!(validate_page_size(Some(200)).expect("maximum"), 200);
        assert_eq!(
            validate_page_size(Some(0)).expect_err("zero").code,
            "invalid_history_request"
        );
        assert_eq!(
            validate_page_size(Some(201))
                .expect_err("over maximum")
                .code,
            "invalid_history_request"
        );
    }

    #[test]
    fn validates_only_opaque_cursor_shape() {
        assert!(valid_history_cursor("history-0000000000000001"));
        assert!(!valid_history_cursor("history-1"));
        assert!(!valid_history_cursor("repository-0000000000000001"));
    }

    #[test]
    fn rejects_an_oversized_frontier() {
        let frontier = (0..=MAX_HISTORY_FRONTIER_TIPS)
            .map(|value| format!("{value:040x}"))
            .collect::<Vec<_>>();
        let error = ensure_frontier_bound(&frontier).expect_err("frontier bound");

        assert_eq!(error.code, "unsupported_repository_state");
    }

    #[test]
    fn stops_a_session_at_the_commit_ceiling() {
        let registry = CommitHistoryRegistry::default();
        let (cursor, has_more, limit_reached) = registry
            .finish_page(test_session(MAX_HISTORY_SESSION_COMMITS))
            .expect("finish bounded session");

        assert!(cursor.is_none());
        assert!(has_more);
        assert!(limit_reached);
        assert!(registry.sessions.lock().expect("session lock").is_empty());
    }

    #[test]
    fn evicts_the_oldest_session_at_the_active_session_bound() {
        let registry = CommitHistoryRegistry::default();
        let mut cursors = Vec::new();
        for _ in 0..=MAX_ACTIVE_HISTORY_SESSIONS {
            cursors.push(
                registry
                    .insert_session(test_session(1))
                    .expect("insert test session"),
            );
        }

        let sessions = registry.sessions.lock().expect("session lock");
        assert_eq!(sessions.len(), MAX_ACTIVE_HISTORY_SESSIONS);
        assert!(!sessions.contains_key(cursors[0].as_str()));
        assert!(sessions.contains_key(cursors.last().expect("newest cursor").as_str()));
    }
}
