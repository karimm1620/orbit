import type {
  CommitHistoryHead,
  CommitRef,
  GraphCommit,
} from "./tauri";

declare const graphLaneIdBrand: unique symbol;
export type GraphLaneId = number & { readonly [graphLaneIdBrand]: true };

export type TopologyLane = {
  laneId: GraphLaneId;
  expectedOid: string;
};

export type GraphEdgeKind =
  | "continuation"
  | "merge"
  | "convergence"
  | "fork"
  | "stub";

export type GraphEdge = {
  commitOid: string;
  parentOid: string | null;
  fromLaneId: GraphLaneId;
  toLaneId: GraphLaneId;
  kind: GraphEdgeKind;
};

export type GraphNode = {
  oid: string;
  laneId: GraphLaneId;
  parentOids: readonly string[];
};

export type GraphRow = {
  commit: GraphCommit;
  node: GraphNode;
  edges: readonly GraphEdge[];
  refs: readonly CommitRef[];
  isHead: boolean;
};

export type GraphContinuationStub = {
  laneId: GraphLaneId;
  expectedOid: string;
};

export type GraphContinuation = {
  activeLanes: readonly TopologyLane[];
  stubs: readonly GraphContinuationStub[];
};

export type TopologyState = {
  activeLanes: readonly TopologyLane[];
  nextLaneId: number;
  processedOids: ReadonlySet<string>;
  refs: readonly CommitRef[];
  head: CommitHistoryHead | null;
};

export type TopologyInput = {
  commits: readonly GraphCommit[];
  refs?: readonly CommitRef[];
  head?: CommitHistoryHead;
};

export type TopologyPageResult = {
  rows: readonly GraphRow[];
  state: TopologyState;
  continuation: GraphContinuation;
};

export function createInitialTopologyState(): TopologyState {
  return {
    activeLanes: [],
    nextLaneId: 1,
    processedOids: new Set(),
    refs: [],
    head: null,
  };
}

/**
 * Reduce Git's children-before-parents sequence into semantic lane transitions.
 *
 * Lane IDs are monotonic and never reused. The active lane array is only an
 * ordered visual-column hint; consumers must use lane IDs for continuity.
 * No renderer coordinates are produced here.
 */
export function reduceTopology(
  input: TopologyInput,
  previousState: TopologyState = createInitialTopologyState(),
): TopologyPageResult {
  const refs = input.refs === undefined ? previousState.refs : sortRefs(input.refs);
  const head = input.head === undefined ? previousState.head : input.head;
  const refsByOid = groupRefs(refs);
  const processedOids = new Set(previousState.processedOids);
  let lanes = previousState.activeLanes.map((lane) => ({ ...lane }));
  let nextLaneId = previousState.nextLaneId;
  const rows: GraphRow[] = [];

  for (const commit of input.commits) {
    if (processedOids.has(commit.oid)) continue;
    processedOids.add(commit.oid);

    const reduction = reduceCommit(commit, lanes, nextLaneId);
    lanes = reduction.lanes;
    nextLaneId = reduction.nextLaneId;

    rows.push({
      commit,
      node: {
        oid: commit.oid,
        laneId: reduction.primaryLaneId,
        parentOids: commit.parentOids,
      },
      edges: reduction.edges,
      refs: refsByOid.get(commit.oid) ?? [],
      isHead: head !== null && head.state !== "unborn" && head.oid === commit.oid,
    });
  }

  const state: TopologyState = {
    activeLanes: lanes,
    nextLaneId,
    processedOids,
    refs,
    head,
  };
  const continuation: GraphContinuation = {
    activeLanes: lanes,
    stubs: lanes.map(({ laneId, expectedOid }) => ({ laneId, expectedOid })),
  };

  return { rows, state, continuation };
}

type CommitReduction = {
  lanes: TopologyLane[];
  nextLaneId: number;
  primaryLaneId: GraphLaneId;
  edges: GraphEdge[];
};

function reduceCommit(
  commit: GraphCommit,
  initialLanes: TopologyLane[],
  initialNextLaneId: number,
): CommitReduction {
  const lanes = initialLanes.map((lane) => ({ ...lane }));
  let nextLaneId = initialNextLaneId;
  const edges: GraphEdge[] = [];
  const matchingIndexes = lanes
    .map((lane, index) => (lane.expectedOid === commit.oid ? index : -1))
    .filter((index) => index >= 0);

  let primaryIndex: number;
  if (matchingIndexes.length === 0) {
    const newLane = createLane(nextLaneId++, commit.oid);
    lanes.push(newLane);
    primaryIndex = lanes.length - 1;
  } else {
    primaryIndex = matchingIndexes[0];
    const primaryLaneId = lanes[primaryIndex].laneId;
    for (const duplicateIndex of matchingIndexes.slice(1).reverse()) {
      edges.push({
        commitOid: commit.oid,
        parentOid: null,
        fromLaneId: lanes[duplicateIndex].laneId,
        toLaneId: primaryLaneId,
        kind: "convergence",
      });
      lanes.splice(duplicateIndex, 1);
      if (duplicateIndex < primaryIndex) primaryIndex -= 1;
    }
  }

  const primaryLaneId = lanes[primaryIndex].laneId;
  let anchorIndex = primaryIndex;
  const firstParent = commit.parentOids[0];

  if (firstParent === undefined) {
    lanes.splice(primaryIndex, 1);
  } else {
    const firstParentIndex = lanes.findIndex(
      (lane, index) => index !== primaryIndex && lane.expectedOid === firstParent,
    );
    if (firstParentIndex === -1) {
      lanes[primaryIndex] = { laneId: primaryLaneId, expectedOid: firstParent };
      anchorIndex = primaryIndex;
      edges.push(edge(commit, primaryLaneId, primaryLaneId, firstParent, "continuation"));
    } else {
      const firstParentLaneId = lanes[firstParentIndex].laneId;
      edges.push(
        edge(commit, primaryLaneId, firstParentLaneId, firstParent, "convergence"),
      );
      lanes.splice(primaryIndex, 1);
      anchorIndex = lanes.findIndex((lane) => lane.laneId === firstParentLaneId);
    }
  }

  for (const parentOid of commit.parentOids.slice(1)) {
    const existingIndex = lanes.findIndex((lane) => lane.expectedOid === parentOid);
    if (existingIndex !== -1) {
      edges.push(
        edge(commit, primaryLaneId, lanes[existingIndex].laneId, parentOid, "merge"),
      );
      continue;
    }

    const newLane = createLane(nextLaneId++, parentOid);
    const insertionIndex = Math.max(0, anchorIndex + 1);
    lanes.splice(insertionIndex, 0, newLane);
    anchorIndex = insertionIndex;
    edges.push(edge(commit, primaryLaneId, newLane.laneId, parentOid, "merge"));
  }

  return { lanes, nextLaneId, primaryLaneId, edges };
}

function createLane(nextLaneId: number, expectedOid: string): TopologyLane {
  return { laneId: nextLaneId as GraphLaneId, expectedOid };
}

function edge(
  commit: GraphCommit,
  fromLaneId: GraphLaneId,
  toLaneId: GraphLaneId,
  parentOid: string,
  kind: GraphEdgeKind,
): GraphEdge {
  return {
    commitOid: commit.oid,
    parentOid,
    fromLaneId,
    toLaneId,
    kind,
  };
}

function sortRefs(refs: readonly CommitRef[]): readonly CommitRef[] {
  return [...refs].sort((left, right) => {
    if (left.fullName < right.fullName) return -1;
    if (left.fullName > right.fullName) return 1;
    if (left.targetOid < right.targetOid) return -1;
    if (left.targetOid > right.targetOid) return 1;
    return refKindRank(left.kind) - refKindRank(right.kind);
  });
}

function groupRefs(refs: readonly CommitRef[]): ReadonlyMap<string, readonly CommitRef[]> {
  const grouped = new Map<string, CommitRef[]>();
  for (const commitRef of refs) {
    const refsForCommit = grouped.get(commitRef.targetOid);
    if (refsForCommit === undefined) {
      grouped.set(commitRef.targetOid, [commitRef]);
    } else {
      refsForCommit.push(commitRef);
    }
  }
  return grouped;
}

function refKindRank(kind: CommitRef["kind"]): number {
  switch (kind) {
    case "localBranch":
      return 0;
    case "remoteTrackingBranch":
      return 1;
    case "lightweightTag":
      return 2;
    case "annotatedTag":
      return 3;
    case "symbolicRef":
      return 4;
  }
}
