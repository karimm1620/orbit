import { describe, expect, it } from "vitest";

import { buildGraphColumns } from "./graphLayout";
import type { GraphCommit } from "./tauri";
import { reduceTopology } from "./topology";

function commit(oid: string, parentOids: readonly string[] = []): GraphCommit {
  return {
    oid,
    shortOid: oid,
    parentOids: [...parentOids],
    subject: oid,
    authorName: "Orbit Tests",
    authorTimestamp: 1_700_000_000,
    committedTimestamp: 1_700_000_000,
  };
}

describe("buildGraphColumns", () => {
  it("keeps an unresolved merge lane visible until the loaded page boundary", () => {
    const first = reduceTopology({
      commits: [
        commit("F", ["C", "E"]),
        commit("C", ["B"]),
        commit("B", ["A"]),
      ],
    });
    const sideLane = first.rows[0].edges.find((edge) => edge.parentOid === "E")
      ?.toLaneId;
    if (sideLane === undefined) throw new Error("expected a secondary-parent lane");

    const firstLayout = buildGraphColumns(first.rows, first.continuation);
    expect(firstLayout.byRow.every((columns) => columns.has(sideLane))).toBe(true);

    const second = reduceTopology(
      {
        commits: [
          commit("E", ["D"]),
          commit("D", ["A"]),
          commit("A"),
        ],
      },
      first.state,
    );
    expect(second.rows[0].node.laneId).toBe(sideLane);
    expect(second.continuation.stubs).toEqual([]);

    const combinedRows = [...first.rows, ...second.rows];
    const combinedLayout = buildGraphColumns(combinedRows, second.continuation);
    expect(combinedLayout.byRow[3].has(sideLane)).toBe(true);
    expect(combinedLayout.byRow[4].has(sideLane)).toBe(true);
    expect(combinedLayout.byRow[5].has(sideLane)).toBe(false);
  });
});
