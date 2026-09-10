import type {
  ChangedFile,
  DiffSide,
  FileDiff,
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
  };
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
