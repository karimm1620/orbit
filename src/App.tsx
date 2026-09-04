import { useRef, useState } from "react";
import "./App.css";
import {
  getRepositorySnapshot,
  type OrbitError,
  type RepositorySnapshot,
  selectRepository,
  toOrbitError,
} from "./lib/tauri";

type RequestState = "idle" | "opening" | "refreshing";

function App() {
  const [repository, setRepository] = useState<RepositorySnapshot | null>(null);
  const [error, setError] = useState<OrbitError | null>(null);
  const [requestState, setRequestState] = useState<RequestState>("idle");
  const requestSequence = useRef(0);

  async function openRepository() {
    const sequence = ++requestSequence.current;
    setRequestState("opening");
    setError(null);

    try {
      const selected = await selectRepository();
      if (sequence !== requestSequence.current) return;
      if (selected) setRepository(selected);
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

    try {
      const refreshed = await getRepositorySnapshot(repository.repositoryId);
      if (sequence === requestSequence.current) setRepository(refreshed);
    } catch (requestError) {
      if (sequence === requestSequence.current) setError(toOrbitError(requestError));
    } finally {
      if (sequence === requestSequence.current) setRequestState("idle");
    }
  }

  const busy = requestState !== "idle";

  return (
    <main className="app-shell" aria-busy={busy}>
      <header className="app-header">
        <div className="brand" aria-label="Orbit">
          <span className="brand-mark" aria-hidden="true">
            ◉
          </span>
          <span>Orbit</span>
        </div>
        <button className="button button-secondary" onClick={openRepository} disabled={busy}>
          {requestState === "opening" ? "Opening…" : "Open Repository"}
        </button>
      </header>

      {error && <ErrorBanner error={error} />}

      {!repository ? (
        <section className="empty-state" aria-labelledby="empty-title">
          <p className="eyebrow">Local repository workspace</p>
          <h1 id="empty-title">See what Git sees.</h1>
          <p>
            Choose a directory inside a Git working tree. Orbit reads it locally with your
            system Git and keeps filesystem access behind its native boundary.
          </p>
          <button className="button button-primary" onClick={openRepository} disabled={busy}>
            {requestState === "opening" ? "Waiting for repository…" : "Open Repository"}
          </button>
          <span className="empty-hint">No account or network connection required.</span>
        </section>
      ) : (
        <RepositoryWorkspace
          repository={repository}
          refreshing={requestState === "refreshing"}
          onRefresh={refreshRepository}
        />
      )}
    </main>
  );
}

function ErrorBanner({ error }: { error: OrbitError }) {
  return (
    <aside className="error-banner" role="alert">
      <div>
        <strong>{error.title}</strong>
        <p>{error.message}</p>
        {error.details && <p className="error-detail">{error.details}</p>}
      </div>
      <code>{error.code}</code>
    </aside>
  );
}

function RepositoryWorkspace({
  repository,
  refreshing,
  onRefresh,
}: {
  repository: RepositorySnapshot;
  refreshing: boolean;
  onRefresh: () => void;
}) {
  const headLabel = repository.head.detached
    ? "Detached HEAD"
    : (repository.head.branch ?? "Unborn branch");
  const oid = repository.head.oid?.slice(0, 8);

  return (
    <div className="workspace">
      <section className="repository-heading">
        <div>
          <p className="eyebrow">Repository</p>
          <h1>{repository.displayName}</h1>
          <p className="repository-path" title={repository.root}>
            {repository.root}
          </p>
        </div>
        <button
          className="button button-secondary"
          onClick={onRefresh}
          disabled={refreshing}
        >
          {refreshing ? "Refreshing…" : "Refresh"}
        </button>
      </section>

      <section className="head-line" aria-label="Current Git state">
        <div>
          <span className={`state-dot ${repository.workingTree.clean ? "clean" : "dirty"}`} />
          <strong>{headLabel}</strong>
          {oid && <code>{oid}</code>}
        </div>
        <span>{repository.workingTree.clean ? "Working tree clean" : "Working tree changed"}</span>
      </section>

      <div className="workspace-grid">
        <section className="history" aria-labelledby="history-title">
          <div className="section-heading">
            <div>
              <p className="eyebrow">Bounded history</p>
              <h2 id="history-title">Recent commits</h2>
            </div>
            <span>{repository.recentCommits.length} shown</span>
          </div>

          {repository.recentCommits.length === 0 ? (
            <div className="history-empty">
              <strong>No commits yet</strong>
              <p>This repository has an unborn HEAD. Its working-tree state is still available.</p>
            </div>
          ) : (
            <ol className="commit-list">
              {repository.recentCommits.map((commit) => (
                <li key={commit.oid}>
                  <span className="commit-node" aria-hidden="true" />
                  <div className="commit-copy">
                    <strong>{commit.subject || "Untitled commit"}</strong>
                    <span>
                      {commit.authorName} · {formatTimestamp(commit.timestamp)}
                    </span>
                  </div>
                  <code title={commit.oid}>{commit.shortOid}</code>
                </li>
              ))}
            </ol>
          )}
        </section>

        <aside className="repository-sidebar">
          <section aria-labelledby="changes-title">
            <p className="eyebrow">Working tree</p>
            <h2 id="changes-title">Change summary</h2>
            <dl className="change-counts">
              <ChangeCount label="Staged" value={repository.workingTree.staged} />
              <ChangeCount label="Unstaged" value={repository.workingTree.unstaged} />
              <ChangeCount label="Untracked" value={repository.workingTree.untracked} />
              <ChangeCount label="Conflicted" value={repository.workingTree.conflicted} alert />
            </dl>
          </section>

          <section className="upstream" aria-labelledby="upstream-title">
            <p className="eyebrow">Tracking</p>
            <h2 id="upstream-title">Upstream</h2>
            {repository.head.upstream ? (
              <>
                <strong>{repository.head.upstream}</strong>
                <p>
                  {repository.head.ahead ?? 0} ahead · {repository.head.behind ?? 0} behind
                </p>
              </>
            ) : (
              <p>No upstream configured.</p>
            )}
          </section>
        </aside>
      </div>
    </div>
  );
}

function ChangeCount({
  label,
  value,
  alert = false,
}: {
  label: string;
  value: number;
  alert?: boolean;
}) {
  return (
    <div className={alert && value > 0 ? "conflict-count" : undefined}>
      <dt>{label}</dt>
      <dd>{value}</dd>
    </div>
  );
}

function formatTimestamp(timestamp: number) {
  const date = new Date(timestamp * 1000);
  if (!Number.isFinite(timestamp) || Number.isNaN(date.getTime())) return "Unknown date";

  return new Intl.DateTimeFormat(undefined, {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(date);
}

export default App;
