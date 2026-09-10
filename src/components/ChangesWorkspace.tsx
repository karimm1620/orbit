import type { ChangesState } from "../lib/changesState";
import {
  availableDiffSides,
  defaultDiffSide,
  groupChanges,
  type ChangeSelection,
} from "../lib/changesState";
import type {
  ChangeFacet,
  ChangedFile,
  DiffHunk,
  DiffLine,
  DiffSide,
  FileDiffContent,
  OrbitError,
} from "../lib/tauri";

type ChangesWorkspaceProps = {
  changes: ChangesState;
  onRefresh: () => void;
  onSelect: (file: ChangedFile, side: DiffSide) => void;
};

export function ChangesWorkspace({ changes, onRefresh, onSelect }: ChangesWorkspaceProps) {
  const retrySelection = () => {
    if (!changes.data || !changes.selected) return;
    const file = changes.data.files.find((entry) => entry.fileId === changes.selected?.fileId);
    if (file) onSelect(file, changes.selected.side);
  };

  return (
    <section className="changes-workspace" aria-label="Repository changes and selected-file diff">
      <ChangesList changes={changes} onRefresh={onRefresh} onSelect={onSelect} />
      <DiffViewer changes={changes} onRetry={retrySelection} />
    </section>
  );
}

function ChangesList({
  changes,
  onRefresh,
  onSelect,
}: {
  changes: ChangesState;
  onRefresh: () => void;
  onSelect: (file: ChangedFile, side: DiffSide) => void;
}) {
  const groups = changes.data ? groupChanges(changes.data.files) : [];

  return (
    <section className="changes-panel" aria-labelledby="changes-title">
      <div className="changes-panel-heading">
        <div>
          <p className="eyebrow">Working tree</p>
          <h2 id="changes-title">Changes</h2>
        </div>
        {changes.data && <span>{changes.data.files.length} files</span>}
      </div>
      {changes.error && (
        <div className="changes-inline-error" role="alert">
          <span>{changes.error.message}</span>
          <button className="text-button" type="button" onClick={onRefresh}>Refresh</button>
        </div>
      )}
      {changes.status === "loading" && !changes.data ? (
        <ChangesLoading />
      ) : !changes.data ? (
        <ChangesUnavailable onRefresh={onRefresh} />
      ) : groups.length === 0 ? (
        <ChangesEmpty />
      ) : (
        <nav className="change-groups" aria-label="Changed files">
          {groups.map((group) => (
            <section className="change-group" key={group.id} aria-labelledby={`change-group-${group.id}`}>
              <div className="change-group-heading">
                <h3 id={`change-group-${group.id}`}>{group.label}</h3>
                <span>{group.files.length}</span>
              </div>
              <ul>
                {group.files.map((file) => (
                  <ChangeFileRow
                    key={file.fileId}
                    file={file}
                    selected={changes.selected}
                    onSelect={onSelect}
                  />
                ))}
              </ul>
            </section>
          ))}
        </nav>
      )}
    </section>
  );
}

function ChangeFileRow({
  file,
  selected,
  onSelect,
}: {
  file: ChangedFile;
  selected: ChangeSelection | null;
  onSelect: (file: ChangedFile, side: DiffSide) => void;
}) {
  const defaultSide = defaultDiffSide(file);
  const sides = availableDiffSides(file);
  const selectedFile = selected?.fileId === file.fileId;
  const description = describeFile(file);

  return (
    <li className={`change-file${selectedFile ? " is-selected" : ""}`}>
      <button
        className="change-file-main"
        type="button"
        aria-pressed={selectedFile}
        onClick={() => defaultSide && onSelect(file, defaultSide)}
      >
        <span className="change-file-copy">
          <strong title={file.path.text}>{file.path.text}</strong>
          {file.originalPath && <small title={file.originalPath.text}>from {file.originalPath.text}</small>}
        </span>
        <span className="change-file-kind">{description}</span>
      </button>
      {file.conflict === null && sides.length > 0 && (
        <span className="change-side-controls" role="group" aria-label={`Diff side for ${file.path.text}`}>
          {sides.map((side) => (
            <button
              className={`change-side-button${selectedFile && selected?.side === side ? " is-selected" : ""}`}
              key={side}
              type="button"
              aria-pressed={selectedFile && selected?.side === side}
              onClick={() => onSelect(file, side)}
            >
              {side === "staged" ? "Staged" : file.unstaged?.kind === "untracked" ? "New" : "Unstaged"}
            </button>
          ))}
        </span>
      )}
    </li>
  );
}

function DiffViewer({ changes, onRetry }: { changes: ChangesState; onRetry: () => void }) {
  const selectedLabel = changes.diff.data?.content.state === "conflict"
    ? "Conflict"
    : changes.selected?.side === "staged"
      ? "Staged"
      : changes.selected
        ? "Unstaged"
        : null;

  return (
    <section className="diff-panel" aria-labelledby="diff-title" aria-busy={changes.diff.status === "loading"}>
      <div className="diff-panel-heading">
        <div>
          <p className="eyebrow">Selected file</p>
          <h2 id="diff-title">Diff</h2>
        </div>
        {selectedLabel && <span>{selectedLabel}</span>}
      </div>
      {changes.diff.status === "loading" ? (
        <DiffLoading />
      ) : changes.diff.error ? (
        <DiffError error={changes.diff.error} onRetry={onRetry} />
      ) : changes.diff.data ? (
        <DiffContent content={changes.diff.data.content} />
      ) : (
        <DiffEmpty />
      )}
    </section>
  );
}

function DiffContent({ content }: { content: FileDiffContent }) {
  switch (content.state) {
    case "text":
      return <TextDiff content={content} />;
    case "binary":
      return <DiffState title="Binary file" message="Git reports binary content. Orbit does not render binary file bytes." />;
    case "conflict":
      return <DiffState title="Conflicted file" message="Orbit keeps combined conflict patches out of this read-only view. Resolve the conflict in your editor or terminal, then refresh." />;
    case "submodule":
      return <DiffState title="Submodule change" message={describeSubmodule(content.submodule)} />;
    case "tooLarge":
      return <DiffState title="Diff is too large" message="This diff exceeded Orbit's bounded viewer limits. No partial patch is shown." />;
    case "unavailable":
      return <DiffState title="Diff unavailable" message={unavailableMessage(content.reason)} />;
  }
}

function TextDiff({
  content,
}: {
  content: Extract<FileDiffContent, { state: "text" }>;
}) {
  return (
    <div className="text-diff">
      <div className="diff-summary" aria-label={`${content.additions} additions and ${content.deletions} deletions`}>
        <span className="diff-additions">+{content.additions}</span>
        <span className="diff-deletions">-{content.deletions}</span>
        <DiffMetadata content={content} />
      </div>
      {content.hunks.length === 0 ? (
        <p className="diff-metadata-only">This change updates file metadata without text hunks.</p>
      ) : (
        <div className="diff-hunks">
          {content.hunks.map((hunk, index) => <DiffHunkView hunk={hunk} key={`${hunk.oldStart}-${hunk.newStart}-${index}`} />)}
        </div>
      )}
    </div>
  );
}

function DiffMetadata({ content }: { content: Extract<FileDiffContent, { state: "text" }> }) {
  const labels: string[] = [];
  if (content.metadata.newFile) labels.push("new file");
  if (content.metadata.deletedFile) labels.push("deleted file");
  if (content.metadata.renamed) labels.push("renamed");
  if (content.metadata.copied) labels.push("copied");
  if (content.metadata.oldMode !== content.metadata.newMode) labels.push("mode changed");
  return labels.length > 0 ? <span className="diff-metadata">{labels.join(", ")}</span> : null;
}

function DiffHunkView({ hunk }: { hunk: DiffHunk }) {
  const label = `Old lines ${hunk.oldStart} through ${hunk.oldStart + Math.max(hunk.oldCount - 1, 0)}, new lines ${hunk.newStart} through ${hunk.newStart + Math.max(hunk.newCount - 1, 0)}`;
  return (
    <section className="diff-hunk" aria-label={label}>
      <div className="diff-hunk-heading">
        <code>@@ -{formatRange(hunk.oldStart, hunk.oldCount)} +{formatRange(hunk.newStart, hunk.newCount)} @@</code>
        {hunk.heading && <span>{hunk.heading}</span>}
      </div>
      <div className="diff-lines">
        {hunk.lines.map((line, index) => <DiffLineView line={line} key={`${line.oldLine}-${line.newLine}-${index}`} />)}
      </div>
    </section>
  );
}

function DiffLineView({ line }: { line: DiffLine }) {
  return (
    <div className={`diff-line diff-line-${line.kind}`}>
      <span className="diff-line-number" aria-hidden="true">{line.oldLine ?? ""}</span>
      <span className="diff-line-number" aria-hidden="true">{line.newLine ?? ""}</span>
      <span className="diff-line-prefix" aria-hidden="true">{line.kind === "addition" ? "+" : line.kind === "deletion" ? "-" : " "}</span>
      <code>{line.content}</code>
      {line.noNewlineAtEnd && <span className="diff-no-newline">No newline at end of file</span>}
    </div>
  );
}

function ChangesLoading() {
  return <div className="changes-loading" aria-live="polite"><span /><span /><span /><p>Reading working tree...</p></div>;
}

function ChangesUnavailable({ onRefresh }: { onRefresh: () => void }) {
  return <div className="changes-empty"><strong>Changes are unavailable</strong><p>Refresh this repository to read its working tree.</p><button className="button button-secondary" type="button" onClick={onRefresh}>Refresh</button></div>;
}

function ChangesEmpty() {
  return <div className="changes-empty"><strong>Working tree clean</strong><p>No staged, unstaged, untracked, or conflicted files are available.</p></div>;
}

function DiffLoading() {
  return <div className="diff-loading" aria-live="polite"><span /><span /><span /><p>Reading selected-file diff...</p></div>;
}

function DiffEmpty() {
  return <DiffState title="Select a changed file" message="Choose a file and side to inspect its bounded Git diff." />;
}

function DiffError({ error, onRetry }: { error: OrbitError; onRetry: () => void }) {
  return <div className="diff-error" role="alert"><strong>{error.title}</strong><p>{error.message}</p><button className="button button-secondary" type="button" onClick={onRetry}>Retry diff</button></div>;
}

function DiffState({ title, message }: { title: string; message: string }) {
  return <div className="diff-state"><strong>{title}</strong><p>{message}</p></div>;
}

function describeFile(file: ChangedFile) {
  if (file.conflict !== null) return "Conflict";
  const facets = [file.staged, file.unstaged].filter((facet): facet is ChangeFacet => facet !== null);
  const kinds = [...new Set(facets.map((facet) => formatKind(facet.kind)))];
  return kinds.join(" and ");
}

function formatKind(kind: ChangeFacet["kind"]) {
  switch (kind) {
    case "typeChanged":
      return "Type changed";
    case "untracked":
      return "Untracked";
    default:
      return `${kind[0].toUpperCase()}${kind.slice(1)}`;
  }
}

function describeSubmodule(submodule: NonNullable<ChangedFile["submodule"]>) {
  const details: string[] = [];
  if (submodule.commitChanged) details.push("commit changed");
  if (submodule.trackedChanges) details.push("tracked changes");
  if (submodule.untrackedChanges) details.push("untracked changes");
  return details.length > 0 ? `Nested repository state: ${details.join(", ")}.` : "Nested repository state is unavailable for this view.";
}

function unavailableMessage(reason: Extract<FileDiffContent, { state: "unavailable" }> ["reason"]) {
  switch (reason) {
    case "stale":
      return "This file changed after the current change set was read. Refresh to inspect the current state.";
    case "sideUnavailable":
      return "That staged or unstaged side is no longer available.";
    case "missingFile":
      return "The selected working-tree file is no longer available.";
    case "unreadableFile":
      return "Orbit could not safely read the selected working-tree file.";
    case "specialFile":
      return "Orbit does not read special filesystem files as diffs.";
    case "unsupportedEncoding":
      return "This text diff is not valid UTF-8, so Orbit does not lossily decode it.";
    case "timeout":
      return "Git did not finish within Orbit's bounded diff deadline.";
  }
}

function formatRange(start: number, count: number) {
  return count === 1 ? `${start}` : `${start},${count}`;
}
