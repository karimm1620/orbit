import type { KeyboardEvent } from "react";

import type { CommitRef } from "../lib/tauri";
import { buildGraphColumns } from "../lib/graphLayout";
import type {
  GraphContinuation,
  GraphEdge,
  GraphLaneId,
  GraphRow,
} from "../lib/topology";

const ROW_HEIGHT = 66;
const LANE_WIDTH = 18;
const GRAPH_PADDING = 18;

type CommitGraphProps = {
  rows: readonly GraphRow[];
  continuation: GraphContinuation;
  selectedOid: string | null;
  onSelect: (oid: string) => void;
};

export function CommitGraph({ rows, continuation, selectedOid, onSelect }: CommitGraphProps) {
  const columns = buildGraphColumns(rows, continuation);
  const graphWidth = Math.min(220, Math.max(86, columns.maxColumns * LANE_WIDTH + GRAPH_PADDING * 2));
  const laneWidth = (graphWidth - GRAPH_PADDING * 2) / columns.maxColumns;

  return (
    <ol className="commit-graph" aria-label="Commit history">
      {rows.map((row, index) => {
        const selected = row.commit.oid === selectedOid;
        return (
          <li key={row.commit.oid}>
            <button
              className={`commit-row${selected ? " is-selected" : ""}`}
              type="button"
              data-commit-row-index={index}
              aria-pressed={selected}
              onClick={() => onSelect(row.commit.oid)}
              onKeyDown={(event) => handleRowKeyDown(event, index, rows.length)}
            >
              <GraphStrip
                row={row}
                columns={columns.byRow[index]}
                graphWidth={graphWidth}
                laneWidth={laneWidth}
              />
              <span className="commit-row-copy">
                <span className="commit-row-topline">
                  <strong className="commit-subject">{row.commit.subject || "Untitled commit"}</strong>
                  {row.isHead && <span className="head-mark">HEAD</span>}
                </span>
                {row.refs.length > 0 && (
                  <span className="ref-list" aria-label="References">
                    {row.refs.map((ref) => <RefBadge key={ref.fullName} ref={ref} />)}
                  </span>
                )}
                <span className="commit-meta">
                  <span>{row.commit.authorName || "Unknown author"}</span>
                  <span>{formatCommitDate(row.commit.committedTimestamp)}</span>
                  {row.commit.parentOids.length > 1 && <span>{row.commit.parentOids.length} parents</span>}
                  <code title={row.commit.oid}>{row.commit.shortOid}</code>
                </span>
              </span>
            </button>
          </li>
        );
      })}
    </ol>
  );
}

function handleRowKeyDown(event: KeyboardEvent<HTMLButtonElement>, index: number, rowCount: number) {
  if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
  event.preventDefault();
  const nextIndex = event.key === "ArrowDown" ? Math.min(index + 1, rowCount - 1) : Math.max(index - 1, 0);
  const nextRow = document.querySelector<HTMLButtonElement>(`[data-commit-row-index="${nextIndex}"]`);
  nextRow?.focus();
}

function GraphStrip({
  row,
  columns,
  graphWidth,
  laneWidth,
}: {
  row: GraphRow;
  columns: Map<GraphLaneId, number>;
  graphWidth: number;
  laneWidth: number;
}) {
  const nodeColumn = columns.get(row.node.laneId) ?? 0;
  const nodeX = GRAPH_PADDING + nodeColumn * laneWidth;

  return (
    <svg
      className="graph-strip"
      width={graphWidth}
      height={ROW_HEIGHT}
      viewBox={`0 0 ${graphWidth} ${ROW_HEIGHT}`}
      aria-hidden="true"
      focusable="false"
    >
      {[...columns.entries()].map(([laneId, column]) => (
        <line
          key={`lane-${laneId}`}
          className={`graph-lane ${laneColorClass(laneId)}`}
          x1={GRAPH_PADDING + column * laneWidth}
          y1="0"
          x2={GRAPH_PADDING + column * laneWidth}
          y2={ROW_HEIGHT}
        />
      ))}
      {row.edges.map((edge, edgeIndex) => (
          <GraphEdgePath
            key={`${edge.parentOid ?? "convergence"}-${edgeIndex}`}
            edge={edge}
            columns={columns}
            laneWidth={laneWidth}
          />
      ))}
      <circle className={`graph-node ${laneColorClass(row.node.laneId)}${row.isHead ? " graph-node-head" : ""}`} cx={nodeX} cy={ROW_HEIGHT / 2} r="5" />
    </svg>
  );
}

function GraphEdgePath({ edge, columns, laneWidth }: { edge: GraphEdge; columns: Map<GraphLaneId, number>; laneWidth: number }) {
  const fromColumn = columns.get(edge.fromLaneId) ?? 0;
  const toColumn = columns.get(edge.toLaneId) ?? 0;
  const fromX = GRAPH_PADDING + fromColumn * laneWidth;
  const toX = GRAPH_PADDING + toColumn * laneWidth;
  const startY = ROW_HEIGHT / 2;
  const endY = ROW_HEIGHT;

  if (fromX === toX) {
    return <line className={`graph-edge graph-edge-${edge.kind} ${laneColorClass(edge.toLaneId)}`} x1={fromX} y1={startY} x2={toX} y2={endY} />;
  }

  const curve = `M ${fromX} ${startY} C ${fromX} ${startY + 10}, ${toX} ${endY - 10}, ${toX} ${endY}`;
  return <path className={`graph-edge graph-edge-${edge.kind} ${laneColorClass(edge.toLaneId)}`} d={curve} />;
}

function laneColorClass(laneId: GraphLaneId) {
  return `graph-color-${(Number(laneId) - 1) % 6}`;
}

function RefBadge({ ref }: { ref: CommitRef }) {
  return <span className={`ref-badge ref-${ref.kind}`} title={ref.fullName}>{ref.displayName}</span>;
}

function formatCommitDate(timestamp: number) {
  const date = new Date(timestamp * 1000);
  if (!Number.isFinite(timestamp) || Number.isNaN(date.getTime())) return "Unknown date";
  return new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" }).format(date);
}
