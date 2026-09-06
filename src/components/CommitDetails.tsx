import type { CommitRef, GraphCommit } from "../lib/tauri";

type CommitDetailsProps = {
  commit: GraphCommit | null;
  refs: readonly CommitRef[];
};

export function CommitDetails({ commit, refs }: CommitDetailsProps) {
  if (!commit) {
    return (
      <section className="details-panel details-empty" aria-labelledby="details-title">
        <p className="eyebrow">Commit details</p>
        <h2 id="details-title">Select a commit</h2>
        <p>Choose a row in the history to inspect its parents, author, and refs.</p>
      </section>
    );
  }

  return (
    <section className="details-panel" aria-labelledby="details-title">
      <p className="eyebrow">Commit details</p>
      <h2 id="details-title">{commit.subject || "Untitled commit"}</h2>
      <dl className="details-list">
        <div>
          <dt>Full OID</dt>
          <dd><code className="oid-value">{commit.oid}</code></dd>
        </div>
        <div>
          <dt>Author</dt>
          <dd>{commit.authorName || "Unknown author"}</dd>
        </div>
        <div>
          <dt>Committed</dt>
          <dd>{formatCommitDate(commit.committedTimestamp)}</dd>
        </div>
        <div>
          <dt>Parents</dt>
          <dd>{commit.parentOids.length === 0 ? "Root commit" : commit.parentOids.length}</dd>
        </div>
      </dl>
      {commit.parentOids.length > 0 && (
        <div className="details-subsection">
          <h3>Parent commits</h3>
          <ul className="oid-list">
            {commit.parentOids.map((oid) => <li key={oid}><code>{oid}</code></li>)}
          </ul>
        </div>
      )}
      {refs.length > 0 && (
        <div className="details-subsection">
          <h3>References</h3>
          <ul className="details-ref-list">
            {refs.map((ref) => <li key={ref.fullName}>{ref.displayName}</li>)}
          </ul>
        </div>
      )}
    </section>
  );
}
function formatCommitDate(timestamp: number) {
  const date = new Date(timestamp * 1000);
  if (!Number.isFinite(timestamp) || Number.isNaN(date.getTime())) return "Unknown date";
  return new Intl.DateTimeFormat(undefined, { dateStyle: "full", timeStyle: "short" }).format(date);
}
