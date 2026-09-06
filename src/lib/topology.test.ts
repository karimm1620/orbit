import { describe, expect, it } from "vitest";

import type { CommitHistoryHead, CommitRef, GraphCommit } from "./tauri";
import {
  createInitialTopologyState,
  reduceTopology,
  type GraphRow,
  type TopologyInput,
  type TopologyPageResult,
} from "./topology";

function commit(oid: string, parentOids: readonly string[] = []): GraphCommit {
  return {
    oid,
    shortOid: oid.slice(0, 12),
    parentOids: [...parentOids],
    subject: oid,
    authorName: "Orbit Tests",
    authorTimestamp: 1_700_000_000,
    committedTimestamp: 1_700_000_000,
  };
}

function topology(
  commits: readonly GraphCommit[],
  options: Pick<TopologyInput, "refs" | "head"> = {},
): TopologyPageResult {
  return reduceTopology({ commits, ...options });
}

function topologyProjection(rows: readonly GraphRow[]) {
  return rows.map((row) => ({
    oid: row.commit.oid,
    laneId: row.node.laneId,
    parentOids: row.node.parentOids,
    edges: row.edges,
  }));
}

function assertInvariants(result: TopologyPageResult): void {
  const rowOids = result.rows.map((row) => row.commit.oid);
  expect(new Set(rowOids).size).toBe(rowOids.length);

  for (const row of result.rows) {
    const parentEdges = row.edges.filter((edge) => edge.parentOid !== null);
    expect(parentEdges.map((edge) => edge.parentOid)).toEqual(row.commit.parentOids);
    expect(parentEdges.every((edge) => edge.commitOid === row.commit.oid)).toBe(true);
    expect(parentEdges.every((edge) => edge.fromLaneId === row.node.laneId)).toBe(true);
  }

  const activeIds = result.state.activeLanes.map((lane) => lane.laneId);
  expect(new Set(activeIds).size).toBe(activeIds.length);
  expect(result.continuation.stubs).toEqual(
    result.state.activeLanes.map(({ laneId, expectedOid }) => ({ laneId, expectedOid })),
  );

  const allocatedLaneIds = new Set<number>();
  for (let laneId = 1; laneId < result.state.nextLaneId; laneId += 1) {
    allocatedLaneIds.add(laneId);
  }
  for (const row of result.rows) {
    expect(allocatedLaneIds.has(row.node.laneId)).toBe(true);
    for (const edge of row.edges) {
      expect(allocatedLaneIds.has(edge.fromLaneId)).toBe(true);
      expect(allocatedLaneIds.has(edge.toLaneId)).toBe(true);
    }
  }
}

describe("reduceTopology", () => {
  it("keeps a linear history in one lane and closes its root", () => {
    const result = topology([
      commit("D", ["C"]),
      commit("C", ["B"]),
      commit("B", ["A"]),
      commit("A"),
    ]);

    expect(result.rows.map((row) => row.node.laneId)).toEqual([1, 1, 1, 1]);
    expect(result.rows.flatMap((row) => row.edges.map((edge) => edge.kind))).toEqual([
      "continuation",
      "continuation",
      "continuation",
    ]);
    expect(result.continuation.stubs).toEqual([]);
    assertInvariants(result);
  });

  it("continues the first parent and opens a deterministic merge lane", () => {
    const result = topology([
      commit("F", ["C", "E"]),
      commit("C", ["B"]),
      commit("E", ["D"]),
      commit("D", ["B"]),
      commit("B", ["A"]),
      commit("A"),
    ]);

    expect(result.rows.map((row) => row.node.laneId)).toEqual([1, 1, 2, 2, 1, 1]);
    expect(result.rows[0].edges).toEqual([
      {
        commitOid: "F",
        parentOid: "C",
        fromLaneId: 1,
        toLaneId: 1,
        kind: "continuation",
      },
      {
        commitOid: "F",
        parentOid: "E",
        fromLaneId: 1,
        toLaneId: 2,
        kind: "merge",
      },
    ]);
    expect(result.rows[3].edges[0]).toMatchObject({
      parentOid: "B",
      fromLaneId: 2,
      toLaneId: 1,
      kind: "convergence",
    });
    assertInvariants(result);
  });

  it("keeps multiple parallel branches distinct until convergence", () => {
    const result = topology([
      commit("B2", ["B1"]),
      commit("C2", ["C1"]),
      commit("B1", ["A"]),
      commit("C1", ["A"]),
      commit("A"),
    ]);

    expect(result.rows.map((row) => row.node.laneId)).toEqual([1, 2, 1, 2, 1]);
    expect(result.rows[3].edges[0]).toMatchObject({
      kind: "convergence",
      parentOid: "A",
      fromLaneId: 2,
      toLaneId: 1,
    });
    assertInvariants(result);
  });

  it("handles multiple merge commits without losing parent relationships", () => {
    const result = topology([
      commit("G", ["E", "F"]),
      commit("E", ["C", "D"]),
      commit("C", ["B"]),
      commit("D", ["B"]),
      commit("F", ["B"]),
      commit("B", ["A"]),
      commit("A"),
    ]);

    expect(result.rows.filter((row) => row.commit.parentOids.length > 1)).toHaveLength(2);
    expect(result.rows.flatMap((row) => row.commit.parentOids)).toEqual([
      "E",
      "F",
      "C",
      "D",
      "B",
      "B",
      "B",
      "A",
    ]);
    assertInvariants(result);
  });

  it("allocates ordered lanes for an octopus merge", () => {
    const result = topology([
      commit("M", ["A", "B", "C"]),
      commit("A"),
      commit("B"),
      commit("C"),
    ]);

    expect(result.rows.map((row) => row.node.laneId)).toEqual([1, 1, 2, 3]);
    expect(result.rows[0].edges.map((edge) => edge.parentOid)).toEqual(["A", "B", "C"]);
    expect(result.rows[0].edges.map((edge) => edge.toLaneId)).toEqual([1, 2, 3]);
    expect(result.rows[0].edges.slice(1).every((edge) => edge.kind === "merge")).toBe(true);
    assertInvariants(result);
  });

  it("emits continuation stubs and reconnects them when a later page arrives", () => {
    const full = topology([
      commit("F", ["C", "E"]),
      commit("C", ["B"]),
      commit("E", ["D"]),
      commit("D", ["B"]),
      commit("B", ["A"]),
      commit("A"),
    ]);
    const first = topology([commit("F", ["C", "E"]), commit("C", ["B"])]);

    expect(first.continuation.stubs).toEqual([
      { laneId: 1, expectedOid: "B" },
      { laneId: 2, expectedOid: "E" },
    ]);

    const second = reduceTopology(
      { commits: [commit("E", ["D"]), commit("D", ["B"]), commit("B", ["A"]), commit("A")] },
      first.state,
    );
    expect(topologyProjection([...first.rows, ...second.rows])).toEqual(
      topologyProjection(full.rows),
    );
    expect(second.continuation.stubs).toEqual([]);
    assertInvariants(first);
    assertInvariants(second);
  });

  it("is equivalent across three pages and preserves lane IDs", () => {
    const pages = [
      [commit("F", ["C", "E"])],
      [commit("C", ["B"]), commit("E", ["D"])],
      [commit("D", ["B"]), commit("B", ["A"]), commit("A")],
    ] as const;
    const oneShot = topology(pages.flat());

    let state = createInitialTopologyState();
    const pagedRows: GraphRow[] = [];
    for (const commits of pages) {
      const page = reduceTopology({ commits }, state);
      pagedRows.push(...page.rows);
      state = page.state;
    }

    expect(topologyProjection(pagedRows)).toEqual(topologyProjection(oneShot.rows));
    expect(pagedRows.map((row) => row.node.laneId)).toEqual([1, 1, 2, 2, 1, 1]);
    expect(state.activeLanes).toEqual([]);
    assertInvariants(oneShot);
  });

  it("keeps finished lane IDs retired when a later disconnected tip appears", () => {
    const first = topology([commit("A")]);
    const second = reduceTopology({ commits: [commit("Z")] }, first.state);

    expect(first.rows[0].node.laneId).toBe(1);
    expect(second.rows[0].node.laneId).toBe(2);
    expect(second.state.nextLaneId).toBe(3);
    assertInvariants(second);
  });

  it("treats refs and detached HEAD as annotations without changing topology", () => {
    const refs: CommitRef[] = [
      {
        kind: "lightweightTag",
        fullName: "refs/tags/release",
        displayName: "release",
        targetOid: "B",
        symbolicTarget: null,
      },
      {
        kind: "localBranch",
        fullName: "refs/heads/main",
        displayName: "main",
        targetOid: "C",
        symbolicTarget: null,
      },
      {
        kind: "remoteTrackingBranch",
        fullName: "refs/remotes/origin/main",
        displayName: "origin/main",
        targetOid: "C",
        symbolicTarget: null,
      },
    ];
    const head: CommitHistoryHead = { state: "detached", oid: "C" };
    const commits = [commit("C", ["B"]), commit("B", ["A"]), commit("A")];
    const withRefs = topology(commits, { refs, head });
    const withoutRefs = topology(commits);

    expect(topologyProjection(withRefs.rows)).toEqual(topologyProjection(withoutRefs.rows));
    expect(withRefs.rows[0].isHead).toBe(true);
    expect(withRefs.rows[0].refs.map((ref) => ref.fullName)).toEqual([
      "refs/heads/main",
      "refs/remotes/origin/main",
    ]);
    expect(withRefs.rows[1].refs[0].fullName).toBe("refs/tags/release");
    assertInvariants(withRefs);
  });

  it("retains refs and HEAD across later pages", () => {
    const refs: CommitRef[] = [
      {
        kind: "localBranch",
        fullName: "refs/heads/main",
        displayName: "main",
        targetOid: "A",
        symbolicTarget: null,
      },
    ];
    const head: CommitHistoryHead = { state: "attached", branch: "main", oid: "B" };
    const first = topology([commit("B", ["A"])], { refs, head });
    const second = reduceTopology({ commits: [commit("A")] }, first.state);

    expect(second.rows[0].refs).toEqual(refs);
    expect(second.rows[0].isHead).toBe(false);
    expect(second.state.head).toEqual(head);
    assertInvariants(second);
  });

  it("is deterministic for identical input and continuation state", () => {
    const input: TopologyInput = {
      commits: [commit("M", ["A", "B"]), commit("A"), commit("B")],
      head: { state: "attached", branch: "main", oid: "M" },
    };
    const left = topology(input.commits, { head: input.head });
    const right = topology(input.commits, { head: input.head });

    expect(topologyProjection(left.rows)).toEqual(topologyProjection(right.rows));
    expect(left.continuation).toEqual(right.continuation);
    expect([...left.state.processedOids]).toEqual([...right.state.processedOids]);
  });
});
