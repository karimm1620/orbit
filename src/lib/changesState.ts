import type {
  ChangedFile,
  DiffSide,
  FileDiff,
  MutationOperation,
  MutationOutcome,
  MutationReceipt,
  OrbitError,
  RepositoryChanges,
  RepositorySnapshot,
} from "./tauri";

export type ChangeSelection = {
  fileId: ChangedFile["fileId"];
  side: DiffSide;
};

export type ChangesState = {
  data: RepositoryChanges | null;
  status: "idle" | "loading";
  error: OrbitError | null;
  changesRequestId: number;
  selected: ChangeSelection | null;
  diff: {
    status: "idle" | "loading";
    error: OrbitError | null;
    data: FileDiff | null;
    requestId: number;
  };
  mutation: {
    status: "idle" | "running";
    requestId: number;
    repositoryId: RepositoryChanges["repositoryId"] | null;
    changeSetId: RepositoryChanges["changeSetId"] | null;
    feedback: MutationFeedback | null;
  };
  stale: boolean;
};

export type MutationFeedback = {
  operation: MutationOperation;
  outcome: MutationOutcome | "stale";
  issue: OrbitError | null;
  refreshRequired: boolean;
  headChanged: boolean;
  commitOid: string | null;
};

export type ChangesGroup = {
  id: "conflicted" | "staged" | "unstaged" | "untracked";
  label: string;
  files: readonly ChangedFile[];
};

export function createEmptyChangesState(): ChangesState {
  return {
    data: null,
    status: "idle",
    error: null,
    changesRequestId: 0,
    selected: null,
    diff: { status: "idle", error: null, data: null, requestId: 0 },
    mutation: { status: "idle", requestId: 0, repositoryId: null, changeSetId: null, feedback: null },
    stale: false,
  };
}

export function workingTreeForChanges(
  state: ChangesState,
  fallback: RepositorySnapshot["workingTree"],
): RepositorySnapshot["workingTree"] {
  return state.data?.summary ?? fallback;
}

export function beginChangesRefresh(state: ChangesState, requestId: number): ChangesState {
  return { ...state, status: "loading", error: null, changesRequestId: requestId };
}

export function completeChangesRefresh(
  state: ChangesState,
  requestId: number,
  data: RepositoryChanges,
): ChangesState {
  if (state.changesRequestId !== requestId) return state;

  return {
    ...state,
    data,
    status: "idle",
    error: null,
    selected: null,
    diff: { status: "idle", error: null, data: null, requestId: state.diff.requestId },
    stale: false,
    mutation: {
      ...state.mutation,
      feedback: state.mutation.feedback?.refreshRequired
        ? { ...state.mutation.feedback, refreshRequired: false }
        : state.mutation.feedback,
    },
  };
}

export function canMutateChanges(state: ChangesState): boolean {
  return state.data !== null && state.status === "idle" && state.mutation.status === "idle" && !state.stale;
}

export function beginMutation(
  state: ChangesState,
  requestId: number,
  operation: MutationOperation,
): ChangesState {
  if (!canMutateChanges(state) || !state.data) return state;

  return {
    ...state,
    changesRequestId: requestId,
    error: null,
    selected: null,
    diff: { status: "idle", error: null, data: null, requestId: state.diff.requestId },
    mutation: {
      status: "running",
      requestId,
      repositoryId: state.data.repositoryId,
      changeSetId: state.data.changeSetId,
      feedback: { operation, outcome: "stale", issue: null, refreshRequired: false, headChanged: false, commitOid: null },
    },
    stale: true,
  };
}

export function completeMutation(
  state: ChangesState,
  requestId: number,
  receipt: MutationReceipt,
): ChangesState {
  const mutation = state.mutation;
  if (
    mutation.status !== "running"
    || mutation.requestId !== requestId
    || mutation.repositoryId !== state.data?.repositoryId
    || mutation.changeSetId !== state.data?.changeSetId
    || receipt.operation !== mutation.feedback?.operation
  ) {
    return state;
  }

  const replacement = receipt.repositoryChanges;
  const replacementMatchesRepository = replacement?.repositoryId === mutation.repositoryId;
  const refreshRequired = receipt.refreshRequired || !replacementMatchesRepository;
  return {
    ...state,
    data: replacementMatchesRepository ? replacement : state.data,
    status: "idle",
    error: null,
    selected: null,
    diff: { status: "idle", error: null, data: null, requestId: state.diff.requestId },
    mutation: {
      status: "idle",
      requestId,
      repositoryId: mutation.repositoryId,
      changeSetId: replacementMatchesRepository ? replacement.changeSetId : mutation.changeSetId,
      feedback: {
        operation: receipt.operation,
        outcome: receipt.outcome,
        issue: receipt.issue ?? null,
        refreshRequired,
        headChanged: receipt.headChanged,
        commitOid: receipt.commitOid ?? null,
      },
    },
    stale: refreshRequired,
  };
}

export function failMutation(
  state: ChangesState,
  requestId: number,
  operation: MutationOperation,
  error: OrbitError,
  stale: boolean,
): ChangesState {
  if (state.mutation.status !== "running" || state.mutation.requestId !== requestId) return state;
  return {
    ...state,
    status: "idle",
    error: null,
    mutation: {
      ...state.mutation,
      status: "idle",
      feedback: { operation, outcome: stale ? "stale" : "rejected", issue: error, refreshRequired: stale, headChanged: false, commitOid: null },
    },
    stale,
  };
}

export function shouldRefreshAfterMutation(receipt: MutationReceipt): boolean {
  return receipt.operation === "createCommit" || receipt.headChanged || receipt.refreshRequired;
}

export function failChangesRefresh(
  state: ChangesState,
  requestId: number,
  error: OrbitError,
): ChangesState {
  if (state.changesRequestId !== requestId) return state;
  return { ...state, status: "idle", error };
}

export function beginDiffLoad(
  state: ChangesState,
  requestId: number,
  selected: ChangeSelection,
): ChangesState {
  return {
    ...state,
    selected,
    diff: { status: "loading", error: null, data: null, requestId },
  };
}

export function completeDiffLoad(
  state: ChangesState,
  requestId: number,
  data: FileDiff,
): ChangesState {
  if (
    state.diff.requestId !== requestId
    || state.data?.changeSetId !== data.changeSetId
    || state.selected?.fileId !== data.fileId
    || state.selected.side !== data.side
  ) {
    return state;
  }

  return { ...state, diff: { status: "idle", error: null, data, requestId } };
}

export function failDiffLoad(
  state: ChangesState,
  requestId: number,
  error: OrbitError,
): ChangesState {
  if (state.diff.requestId !== requestId) return state;
  return { ...state, diff: { ...state.diff, status: "idle", error } };
}

export function availableDiffSides(file: ChangedFile): readonly DiffSide[] {
  const sides: DiffSide[] = [];
  if (file.staged !== null) sides.push("staged");
  if (file.unstaged !== null) sides.push("unstaged");

  // Rust returns the typed conflict state before examining the requested side.
  // This remains a closed enum, never a frontend Git expression.
  return sides.length > 0 ? sides : file.conflict !== null ? ["staged"] : [];
}

export function defaultDiffSide(file: ChangedFile): DiffSide | null {
  return availableDiffSides(file)[0] ?? null;
}

export function groupChanges(files: readonly ChangedFile[]): readonly ChangesGroup[] {
  const conflicted = files.filter((file) => file.conflict !== null);
  const staged = files.filter((file) => file.conflict === null && file.staged !== null);
  const unstaged = files.filter(
    (file) =>
      file.conflict === null
      && file.staged === null
      && file.unstaged !== null
      && file.unstaged.kind !== "untracked",
  );
  const untracked = files.filter(
    (file) =>
      file.conflict === null
      && file.staged === null
      && file.unstaged?.kind === "untracked",
  );

  const groups: ChangesGroup[] = [
    { id: "conflicted", label: "Conflicted", files: conflicted },
    { id: "staged", label: "Staged", files: staged },
    { id: "unstaged", label: "Unstaged", files: unstaged },
    { id: "untracked", label: "Untracked", files: untracked },
  ];
  return groups.filter((group) => group.files.length > 0);
}
