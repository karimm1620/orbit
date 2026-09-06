import type {
  GraphContinuation,
  GraphLaneId,
  GraphRow,
} from "./topology";

export type GraphColumnLayout = {
  maxColumns: number;
  byRow: Array<Map<GraphLaneId, number>>;
};

/**
 * Assign compact presentation columns without changing semantic lane IDs.
 * Active continuation lanes are pinned through the last loaded row so an
 * unresolved parent remains visible until a later page reconnects it.
 */
export function buildGraphColumns(
  rows: readonly GraphRow[],
  continuation: GraphContinuation,
): GraphColumnLayout {
  const lastUse = new Map<GraphLaneId, number>();
  rows.forEach((row, index) => {
    lastUse.set(row.node.laneId, index);
    for (const edge of row.edges) {
      lastUse.set(edge.fromLaneId, index);
      lastUse.set(edge.toLaneId, index);
    }
  });

  const lastRowIndex = rows.length - 1;
  for (const stub of continuation.stubs) {
    if (lastUse.has(stub.laneId)) {
      lastUse.set(stub.laneId, lastRowIndex);
    }
  }

  const activeColumns: GraphLaneId[] = [];
  const byRow: Array<Map<GraphLaneId, number>> = [];
  let maxColumns = 1;

  rows.forEach((row, index) => {
    const used = [
      row.node.laneId,
      ...row.edges.flatMap((edge) => [edge.fromLaneId, edge.toLaneId]),
    ];
    for (const laneId of used) {
      if (!activeColumns.includes(laneId)) activeColumns.push(laneId);
    }

    const columnMap = new Map<GraphLaneId, number>();
    activeColumns.forEach((laneId, column) => columnMap.set(laneId, column));
    byRow.push(columnMap);
    maxColumns = Math.max(maxColumns, activeColumns.length);

    for (let column = activeColumns.length - 1; column >= 0; column -= 1) {
      const laneId = activeColumns[column];
      if (lastUse.get(laneId) === index) activeColumns.splice(column, 1);
    }
  });

  return { maxColumns, byRow };
}
