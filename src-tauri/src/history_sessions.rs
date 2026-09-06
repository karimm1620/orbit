use std::{
    collections::HashMap,
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
        describe_head, history_starting_tips, read_commit_refs, read_graph_commit_page,
        read_graph_order, read_object_format, CommitHistoryHead, CommitRef, GitRunner, GraphCommit,
        HeadSnapshot, ObjectFormat,
    },
};

pub const DEFAULT_HISTORY_PAGE_SIZE: usize = 100;
pub const MAX_HISTORY_PAGE_SIZE: usize = 200;
pub const MAX_HISTORY_STARTING_TIPS: usize = 512;
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

#[derive(Debug)]
struct CommitHistorySession {
    repository_id: String,
    root: PathBuf,
    head: CommitHistoryHead,
    object_format: ObjectFormat,
    ordered_oids: Vec<String>,
    next_index: usize,
    history_truncated: bool,
    last_access: Instant,
}

enum HistorySessionSlot {
    Available(CommitHistorySession),
    InFlight {
        repository_id: String,
        root: PathBuf,
        last_access: Instant,
    },
}

impl HistorySessionSlot {
    fn repository_id(&self) -> &str {
        match self {
            Self::Available(session) => &session.repository_id,
            Self::InFlight { repository_id, .. } => repository_id,
        }
    }

    fn root(&self) -> &Path {
        match self {
            Self::Available(session) => &session.root,
            Self::InFlight { root, .. } => root,
        }
    }

    fn last_access(&self) -> Instant {
        match self {
            Self::Available(session) => session.last_access,
            Self::InFlight { last_access, .. } => *last_access,
        }
    }
}

pub struct CommitHistoryRegistry {
    sessions: Mutex<HashMap<String, HistorySessionSlot>>,
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
        sessions.retain(|_, session| session.repository_id() != repository_id);
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
        let starting_tips = history_starting_tips(&head, &refs);
        ensure_starting_tip_bound(&starting_tips)?;

        if starting_tips.is_empty() {
            return Ok(CommitHistoryPage {
                commits: Vec::new(),
                refs,
                head,
                next_cursor: None,
                has_more: false,
                session_limit_reached: false,
            });
        }

        let mut ordered_oids = read_graph_order(
            runner,
            root,
            &starting_tips,
            MAX_HISTORY_SESSION_COMMITS + 1,
            object_format,
        )?;
        let history_truncated = ordered_oids.len() > MAX_HISTORY_SESSION_COMMITS;
        ordered_oids.truncate(MAX_HISTORY_SESSION_COMMITS);
        let first_page_end = page_size.min(ordered_oids.len());
        let commits =
            read_graph_commit_page(runner, root, &ordered_oids[..first_page_end], object_format)?;
        let session = CommitHistorySession {
            repository_id: repository_id.to_owned(),
            root: root.to_owned(),
            head: head.clone(),
            object_format,
            ordered_oids,
            next_index: first_page_end,
            history_truncated,
            last_access: Instant::now(),
        };
        let (next_cursor, has_more, session_limit_reached) = self.finish_initial_page(session)?;

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
        let mut session = self.begin_continuation(repository_id, root, cursor)?;
        if let Err(error) = runner.require_no_lazy_fetch() {
            self.restore_continuation(cursor, session)?;
            return Err(error);
        }

        let page_end = session
            .next_index
            .saturating_add(page_size)
            .min(session.ordered_oids.len());
        if page_end == session.next_index {
            self.restore_continuation(cursor, session)?;
            return Err(OrbitError::history_session_unavailable());
        }
        let commits = match read_graph_commit_page(
            runner,
            root,
            &session.ordered_oids[session.next_index..page_end],
            session.object_format,
        ) {
            Ok(commits) => commits,
            Err(error) => {
                self.restore_continuation(cursor, session)?;
                return Err(error);
            }
        };
        session.next_index = page_end;
        let head = session.head.clone();
        let (next_cursor, has_more, session_limit_reached) =
            self.finish_continuation(cursor, session)?;

        Ok(CommitHistoryPage {
            commits,
            refs: Vec::new(),
            head,
            next_cursor,
            has_more,
            session_limit_reached,
        })
    }

    fn finish_initial_page(
        &self,
        mut session: CommitHistorySession,
    ) -> Result<(Option<HistoryCursor>, bool, bool), OrbitError> {
        let (has_more, session_limit_reached) = page_state(&session);
        let next_cursor = if has_more && !session_limit_reached {
            session.last_access = Instant::now();
            Some(self.insert_session(session)?)
        } else {
            None
        };

        Ok((next_cursor, has_more, session_limit_reached))
    }

    fn finish_continuation(
        &self,
        cursor: &str,
        mut session: CommitHistorySession,
    ) -> Result<(Option<HistoryCursor>, bool, bool), OrbitError> {
        let (has_more, session_limit_reached) = page_state(&session);
        let next_cursor = (has_more && !session_limit_reached).then(|| self.new_cursor());
        session.last_access = Instant::now();

        let mut sessions = self.lock_sessions()?;
        let in_flight = sessions.get(cursor).is_some_and(|slot| {
            matches!(slot, HistorySessionSlot::InFlight { .. })
                && slot.repository_id() == session.repository_id
                && slot.root() == session.root
        });
        if !in_flight {
            return Err(OrbitError::history_session_unavailable());
        }
        sessions.remove(cursor);
        if let Some(next_cursor) = &next_cursor {
            sessions.insert(
                next_cursor.0.clone(),
                HistorySessionSlot::Available(session),
            );
        }

        Ok((next_cursor, has_more, session_limit_reached))
    }

    fn insert_session(&self, session: CommitHistorySession) -> Result<HistoryCursor, OrbitError> {
        let cursor = self.new_cursor();
        let mut sessions = self.lock_sessions()?;
        prune_expired(&mut sessions, Instant::now());

        if sessions.len() >= MAX_ACTIVE_HISTORY_SESSIONS {
            let oldest = sessions
                .iter()
                .filter(|(_, session)| matches!(session, HistorySessionSlot::Available(_)))
                .min_by(|(left_cursor, left), (right_cursor, right)| {
                    left.last_access()
                        .cmp(&right.last_access())
                        .then_with(|| left_cursor.cmp(right_cursor))
                })
                .map(|(cursor, _)| cursor.clone());
            if let Some(oldest) = oldest {
                sessions.remove(&oldest);
            } else {
                return Err(OrbitError::history_session_unavailable());
            }
        }

        sessions.insert(cursor.0.clone(), HistorySessionSlot::Available(session));
        Ok(cursor)
    }

    fn begin_continuation(
        &self,
        repository_id: &str,
        root: &Path,
        cursor: &str,
    ) -> Result<CommitHistorySession, OrbitError> {
        if !valid_history_cursor(cursor) {
            return Err(OrbitError::history_session_unavailable());
        }

        let now = Instant::now();
        let mut sessions = self.lock_sessions()?;
        prune_expired(&mut sessions, now);
        let slot = sessions
            .get_mut(cursor)
            .ok_or_else(OrbitError::history_session_unavailable)?;
        if !matches!(slot, HistorySessionSlot::Available(_))
            || slot.repository_id() != repository_id
            || slot.root() != root
        {
            return Err(OrbitError::history_session_unavailable());
        }

        let in_flight = HistorySessionSlot::InFlight {
            repository_id: repository_id.to_owned(),
            root: root.to_owned(),
            last_access: now,
        };
        match std::mem::replace(slot, in_flight) {
            HistorySessionSlot::Available(mut session) => {
                session.last_access = now;
                Ok(session)
            }
            HistorySessionSlot::InFlight { .. } => Err(OrbitError::history_session_unavailable()),
        }
    }

    fn restore_continuation(
        &self,
        cursor: &str,
        mut session: CommitHistorySession,
    ) -> Result<(), OrbitError> {
        let mut sessions = self.lock_sessions()?;
        let slot = sessions
            .get_mut(cursor)
            .ok_or_else(OrbitError::history_session_unavailable)?;
        if !matches!(slot, HistorySessionSlot::InFlight { .. })
            || slot.repository_id() != session.repository_id
            || slot.root() != session.root
        {
            return Err(OrbitError::history_session_unavailable());
        }

        session.last_access = Instant::now();
        *slot = HistorySessionSlot::Available(session);
        Ok(())
    }

    fn lock_sessions(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, HashMap<String, HistorySessionSlot>>, OrbitError> {
        self.sessions.lock().map_err(|_| {
            OrbitError::internal(
                "read_commit_history",
                "Commit-history session state is unavailable.",
            )
        })
    }

    fn new_cursor(&self) -> HistoryCursor {
        let value = self.next_cursor.fetch_add(1, Ordering::Relaxed);
        HistoryCursor(format!("history-{value:016x}"))
    }

    #[cfg(test)]
    pub fn expire_for_test(&self, cursor: &str) {
        let mut sessions = self.sessions.lock().expect("history session lock");
        let expired = Instant::now()
            .checked_sub(HISTORY_SESSION_IDLE_TTL + Duration::from_secs(1))
            .expect("test instant supports subtraction");
        if let Some(slot) = sessions.get_mut(cursor) {
            match slot {
                HistorySessionSlot::Available(session) => session.last_access = expired,
                HistorySessionSlot::InFlight { last_access, .. } => *last_access = expired,
            }
        }
    }
}

fn page_state(session: &CommitHistorySession) -> (bool, bool) {
    let buffered_history_remains = session.next_index < session.ordered_oids.len();
    let session_limit_reached = !buffered_history_remains && session.history_truncated;
    (
        buffered_history_remains || session.history_truncated,
        session_limit_reached,
    )
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

fn ensure_starting_tip_bound(starting_tips: &[String]) -> Result<(), OrbitError> {
    if starting_tips.len() > MAX_HISTORY_STARTING_TIPS {
        Err(OrbitError::unsupported(
            "read_commit_history",
            format!(
                "This repository has more than {MAX_HISTORY_STARTING_TIPS} active history tips."
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

fn prune_expired(sessions: &mut HashMap<String, HistorySessionSlot>, now: Instant) {
    sessions.retain(|_, session| {
        now.checked_duration_since(session.last_access())
            .is_some_and(|idle| idle < HISTORY_SESSION_IDLE_TTL)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_session(next_index: usize) -> CommitHistorySession {
        CommitHistorySession {
            repository_id: "repository-0000000000000001".to_owned(),
            root: PathBuf::from("/test/repository"),
            head: CommitHistoryHead::Detached {
                oid: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
            },
            object_format: ObjectFormat::Sha1,
            ordered_oids: (0..MAX_HISTORY_SESSION_COMMITS)
                .map(|value| format!("{value:040x}"))
                .collect(),
            next_index,
            history_truncated: true,
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
    fn rejects_too_many_starting_tips() {
        let starting_tips = (0..=MAX_HISTORY_STARTING_TIPS)
            .map(|value| format!("{value:040x}"))
            .collect::<Vec<_>>();
        let error = ensure_starting_tip_bound(&starting_tips).expect_err("starting-tip bound");

        assert_eq!(error.code, "unsupported_repository_state");
    }

    #[test]
    fn stops_a_session_at_the_commit_ceiling() {
        let registry = CommitHistoryRegistry::default();
        let (cursor, has_more, limit_reached) = registry
            .finish_initial_page(test_session(MAX_HISTORY_SESSION_COMMITS))
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

    #[test]
    fn active_session_eviction_preserves_an_in_flight_continuation() {
        let registry = CommitHistoryRegistry::default();
        let in_flight_cursor = registry
            .insert_session(test_session(1))
            .expect("insert in-flight test session");
        let in_flight_session = registry
            .begin_continuation(
                "repository-0000000000000001",
                Path::new("/test/repository"),
                in_flight_cursor.as_str(),
            )
            .expect("begin in-flight continuation");

        for _ in 1..MAX_ACTIVE_HISTORY_SESSIONS {
            registry
                .insert_session(test_session(1))
                .expect("fill active-session registry");
        }
        registry
            .insert_session(test_session(1))
            .expect("evict an available session");

        assert!(matches!(
            registry
                .sessions
                .lock()
                .expect("session lock")
                .get(in_flight_cursor.as_str()),
            Some(HistorySessionSlot::InFlight { .. })
        ));
        registry
            .restore_continuation(in_flight_cursor.as_str(), in_flight_session)
            .expect("restore in-flight test session");
    }

    #[test]
    fn an_in_flight_cursor_rejects_concurrent_use_and_rotates_after_success() {
        let registry = CommitHistoryRegistry::default();
        let old_cursor = registry
            .insert_session(test_session(1))
            .expect("insert test session");
        let mut session = registry
            .begin_continuation(
                "repository-0000000000000001",
                Path::new("/test/repository"),
                old_cursor.as_str(),
            )
            .expect("begin continuation");

        let concurrent = registry
            .begin_continuation(
                "repository-0000000000000001",
                Path::new("/test/repository"),
                old_cursor.as_str(),
            )
            .expect_err("in-flight cursor cannot be reused");
        assert_eq!(concurrent.code, "history_session_unavailable");

        session.next_index += 1;
        let (next_cursor, has_more, limit_reached) = registry
            .finish_continuation(old_cursor.as_str(), session)
            .expect("finish continuation");
        assert!(has_more);
        assert!(!limit_reached);
        let next_cursor = next_cursor.expect("rotated cursor");

        assert!(registry
            .begin_continuation(
                "repository-0000000000000001",
                Path::new("/test/repository"),
                old_cursor.as_str(),
            )
            .is_err());
        let next_session = registry
            .begin_continuation(
                "repository-0000000000000001",
                Path::new("/test/repository"),
                next_cursor.as_str(),
            )
            .expect("new cursor is available");
        registry
            .restore_continuation(next_cursor.as_str(), next_session)
            .expect("restore test continuation");
    }

    #[cfg(unix)]
    #[test]
    fn a_capability_failure_restores_the_cursor_for_retry() {
        let registry = CommitHistoryRegistry::default();
        let cursor = registry
            .insert_session(test_session(1))
            .expect("insert test session");
        let error = registry
            .load_more(
                &GitRunner::with_executable("false"),
                "repository-0000000000000001",
                Path::new("/test/repository"),
                cursor.as_str(),
                Some(1),
            )
            .expect_err("capability check should fail");
        assert_eq!(error.code, "git_capability_unavailable");

        let session = registry
            .begin_continuation(
                "repository-0000000000000001",
                Path::new("/test/repository"),
                cursor.as_str(),
            )
            .expect("failed continuation keeps cursor available");
        registry
            .restore_continuation(cursor.as_str(), session)
            .expect("restore test continuation");
    }
}
