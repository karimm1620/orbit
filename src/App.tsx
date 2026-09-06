import { useMemo, useRef, useState } from "react";
import "./App.css";
import { CommitDetails } from "./components/CommitDetails";
import { CommitGraph } from "./components/CommitGraph";
import {
  getCommitHistoryPage,
  getRepositorySnapshot,
  type CommitHistoryHead,
  type CommitHistoryPage,
  type HistoryCursor,
  type OrbitError,
  type RepositorySnapshot,
  selectRepository,
  toOrbitError,
} from "./lib/tauri";
import { createInitialTopologyState, reduceTopology, type GraphContinuation, type GraphRow, type TopologyState } from "./lib/topology";

type RequestState = "idle" | "opening" | "refreshing";
type HistoryRequestState = "idle" | "loading" | "loading-more";

type HistoryState = {
  rows: readonly GraphRow[];
  topology: TopologyState;
  continuation: GraphContinuation;
  cursor: HistoryCursor | null;
  hasMore: boolean;
  sessionLimitReached: boolean;
  head: CommitHistoryHead | null;
  status: HistoryRequestState;
  error: OrbitError | null;
};

function createEmptyHistory(): HistoryState {
  return { rows: [], topology: createInitialTopologyState(), continuation: { activeLanes: [], stubs: [] }, cursor: null, hasMore: false, sessionLimitReached: false, head: null, status: "idle", error: null };
}

function App() {
  const [repository, setRepository] = useState<RepositorySnapshot | null>(null);
  const [history, setHistory] = useState<HistoryState>(createEmptyHistory);
  const [selectedOid, setSelectedOid] = useState<string | null>(null);
  const [error, setError] = useState<OrbitError | null>(null);
  const [requestState, setRequestState] = useState<RequestState>("idle");
  const requestSequence = useRef(0);

  async function openRepository() {
    const sequence = ++requestSequence.current;
    setRequestState("opening");
    setError(null);
    setHistory((current) => current.status === "loading-more" ? { ...current, status: "idle" } : current);
    try {
      const selected = await selectRepository();
      if (sequence !== requestSequence.current || !selected) return;
      setRepository(selected);
      setSelectedOid(null);
      await loadInitialHistory(selected, sequence);
    } catch (requestError) {
      if (sequence === requestSequence.current) setError(toOrbitError(requestError));
    } finally {
      if (sequence === requestSequence.current) setRequestState("idle");
    }
  }

  async function refreshRepository() {
    if (!repository) return;
    const sequence = ++requestSequence.current;
    setRequestState("refreshing");
    setError(null);
    setSelectedOid(null);
    setHistory((current) => ({ ...current, status: "loading", error: null }));
    try {
      const refreshed = await getRepositorySnapshot(repository.repositoryId);
      if (sequence !== requestSequence.current) return;
      setRepository(refreshed);
      await loadInitialHistory(refreshed, sequence);
    } catch (requestError) {
      if (sequence === requestSequence.current) {
        const orbitError = toOrbitError(requestError);
        setError(orbitError);
        setHistory((current) => ({ ...current, status: "idle", error: orbitError }));
      }
    } finally {
      if (sequence === requestSequence.current) setRequestState("idle");
    }
  }

  async function loadInitialHistory(snapshot: RepositorySnapshot, sequence: number) {
    setHistory({ ...createEmptyHistory(), status: "loading" });
    try {
      const page = await getCommitHistoryPage(snapshot.repositoryId);
      if (sequence !== requestSequence.current) return;
      setHistory(historyFromPage(page, createInitialTopologyState()));
    } catch (requestError) {
      if (sequence === requestSequence.current) setHistory({ ...createEmptyHistory(), error: toOrbitError(requestError) });
    }
  }

  async function loadMoreHistory() {
    if (!repository || history.status !== "idle" || !history.hasMore || !history.cursor) return;
    const sequence = requestSequence.current;
    const cursor = history.cursor;
    const priorTopology = history.topology;
    const priorRows = history.rows;
    setHistory((current) => ({ ...current, status: "loading-more", error: null }));
    try {
      const page = await getCommitHistoryPage(repository.repositoryId, cursor);
      if (sequence !== requestSequence.current) return;
      setHistory(historyFromPage(page, priorTopology, priorRows));
    } catch (requestError) {
      if (sequence === requestSequence.current) setHistory((current) => ({ ...current, status: "idle", error: toOrbitError(requestError) }));
    }
  }

  const selectedCommit = useMemo(() => history.rows.find((row) => row.commit.oid === selectedOid)?.commit ?? null, [history.rows, selectedOid]);
  const selectedRefs = useMemo(() => history.rows.find((row) => row.commit.oid === selectedOid)?.refs ?? [], [history.rows, selectedOid]);
  const busy = requestState !== "idle";

  return (
    <main className="app-shell" aria-busy={busy}>
      <header className="app-header">
        <div className="brand" aria-label="Orbit"><span className="brand-mark" aria-hidden="true">O</span><span>Orbit</span></div>
        <button className="button button-secondary" onClick={openRepository} disabled={busy}>{requestState === "opening" ? "Opening..." : "Open repository"}</button>
      </header>
      {error && <ErrorBanner error={error} />}
      {!repository ? <EmptyState busy={busy} onOpen={openRepository} /> : (
        <RepositoryWorkspace
          repository={repository}
          history={history}
          selectedOid={selectedOid}
          selectedCommit={selectedCommit}
          selectedRefs={selectedRefs}
          refreshing={requestState === "refreshing"}
          onRefresh={refreshRepository}
          onSelect={setSelectedOid}
          onLoadMore={loadMoreHistory}
        />
      )}
    </main>
  );
}

function EmptyState({ busy, onOpen }: { busy: boolean; onOpen: () => void }) {
  return <section className="empty-state" aria-labelledby="empty-title"><p className="eyebrow">Local Git workspace</p><h1 id="empty-title">Read your repository clearly.</h1><p>Open a local Git repository to inspect its state and history without an account or network connection.</p><button className="button button-primary" onClick={onOpen} disabled={busy}>{busy ? "Waiting for repository..." : "Open repository"}</button><span className="empty-hint">Orbit uses your system Git and keeps filesystem access native.</span></section>;
}

function ErrorBanner({ error }: { error: OrbitError }) {
  return <aside className="error-banner" role="alert"><div><strong>{error.title}</strong><p>{error.message}</p>{error.details && <p className="error-detail">{error.details}</p>}</div><code>{error.code}</code></aside>;
}

function RepositoryWorkspace({
  repository, history, selectedOid, selectedCommit, selectedRefs, refreshing, onRefresh, onSelect, onLoadMore,
}: {
  repository: RepositorySnapshot;
  history: HistoryState;
  selectedOid: string | null;
  selectedCommit: GraphRow["commit"] | null;
  selectedRefs: readonly GraphRow["refs"][number][];
  refreshing: boolean;
  onRefresh: () => void;
  onSelect: (oid: string) => void;
  onLoadMore: () => void;
}) {
  const headLabel = repository.head.detached ? "Detached HEAD" : (repository.head.branch ?? "Unborn branch");
  const oid = repository.head.oid?.slice(0, 8);
  return (
    <div className="workspace">
      <section className="repository-heading"><div><p className="eyebrow">Repository</p><h1>{repository.displayName}</h1><p className="repository-path" title={repository.root}>{repository.root}</p></div><button className="button button-secondary" onClick={onRefresh} disabled={refreshing}>{refreshing ? "Refreshing..." : "Refresh"}</button></section>
      <section className="state-rail" aria-label="Current Git state"><div className="state-identity"><span className={`state-dot ${repository.workingTree.clean ? "clean" : "dirty"}`} aria-hidden="true" /><strong>{headLabel}</strong>{oid && <code>{oid}</code>}</div><div className="state-summary"><span>{repository.workingTree.clean ? "Working tree clean" : "Working tree changed"}</span><span>{repository.workingTree.staged + repository.workingTree.unstaged + repository.workingTree.untracked} changes</span></div></section>
      <section className="change-strip" aria-label="Working tree summary"><ChangeCount label="Staged" value={repository.workingTree.staged} /><ChangeCount label="Unstaged" value={repository.workingTree.unstaged} /><ChangeCount label="Untracked" value={repository.workingTree.untracked} /><ChangeCount label="Conflicted" value={repository.workingTree.conflicted} alert /><div className="upstream-summary"><span>Upstream</span><strong>{repository.head.upstream ?? "Not configured"}</strong>{repository.head.upstream && <small>{repository.head.ahead ?? 0} ahead, {repository.head.behind ?? 0} behind</small>}</div></section>
      <div className="workspace-grid">
        <section className="history-panel" aria-labelledby="history-title"><div className="section-heading"><div><p className="eyebrow">History</p><h2 id="history-title">Commit graph</h2></div><span>{history.rows.length} loaded</span></div>{history.status === "loading" && history.rows.length === 0 ? <HistoryLoading /> : history.error && history.rows.length === 0 ? <HistoryError error={history.error} onRetry={onRefresh} /> : history.rows.length === 0 ? <HistoryEmpty /> : <><CommitGraph rows={history.rows} continuation={history.continuation} selectedOid={selectedOid} onSelect={onSelect} /><HistoryFooter history={history} onLoadMore={onLoadMore} onRetry={onRefresh} /></>}</section>
        <CommitDetails commit={selectedCommit} refs={selectedRefs} />
      </div>
    </div>
  );
}

function historyFromPage(page: CommitHistoryPage, previousTopology: TopologyState, previousRows: readonly GraphRow[] = []): HistoryState {
  const topologyInput = { commits: page.commits, head: page.head, ...(page.refs.length > 0 ? { refs: page.refs } : {}) };
  const reduced = reduceTopology(topologyInput, previousTopology);
  return { rows: [...previousRows, ...reduced.rows], topology: reduced.state, continuation: reduced.continuation, cursor: page.nextCursor, hasMore: page.hasMore, sessionLimitReached: page.sessionLimitReached, head: page.head, status: "idle", error: null };
}

function HistoryLoading() {
  return <div className="history-loading" aria-live="polite"><span className="loading-line loading-line-wide" /><span className="loading-line" /><span className="loading-line loading-line-short" /><p>Reading bounded history...</p></div>;
}

function HistoryEmpty() {
  return <div className="history-empty"><strong>No commits yet</strong><p>This repository has an unborn HEAD. Its working-tree state is still available.</p></div>;
}

function HistoryError({ error, onRetry }: { error: OrbitError; onRetry: () => void }) {
  return <div className="history-error" role="alert"><strong>{error.title}</strong><p>{error.message}</p><button className="button button-secondary" onClick={onRetry}>Retry history</button></div>;
}

function HistoryFooter({ history, onLoadMore, onRetry }: { history: HistoryState; onLoadMore: () => void; onRetry: () => void }) {
  return <div className="history-footer">{history.error && <div className="history-load-error" role="alert"><span>{history.error.message}</span><button className="text-button" onClick={onRetry}>Retry</button></div>}{history.sessionLimitReached ? <p className="history-end">History session limit reached. Refresh to start a new snapshot.</p> : history.hasMore ? <><p className="history-continuation">Graph continues below the loaded history.</p><button className="button button-secondary load-more" onClick={onLoadMore} disabled={history.status !== "idle"}>{history.status === "loading-more" ? "Loading older commits..." : "Load older commits"}</button></> : <p className="history-end">End of available history</p>}</div>;
}

function ChangeCount({ label, value, alert = false }: { label: string; value: number; alert?: boolean }) {
  return <div className={alert && value > 0 ? "conflict-count" : undefined}><span>{label}</span><strong>{value}</strong></div>;
}

export default App;
