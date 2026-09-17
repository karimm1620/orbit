import { describe, expect, it } from "vitest";

import {
  availableDiffSides,
  beginMutation,
  beginChangesRefresh,
  beginDiffLoad,
  canMutateChanges,
  completeMutation,
  completeChangesRefresh,
  completeDiffLoad,
  createEmptyChangesState,
  failChangesRefresh,
  failDiffLoad,
  failMutation,
  groupChanges,
  shouldRefreshAfterMutation,
  workingTreeForChanges,
} from "./changesState";
import type { ChangedFile, FileDiff, MutationReceipt, OrbitError, RepositoryChanges } from "./tauri";

const error: OrbitError = {
  code: "repository_unavailable",
  title: "Repository unavailable",
  message: "The repository is unavailable.",
  operation: "get_repository_changes",
  recoverable: true,
};

function file(id: string, options: Partial<ChangedFile> = {}): ChangedFile {
  return {
    fileId: id as ChangedFile["fileId"],
    path: { text: `${id}.txt`, escaped: false },
    originalPath: null,
    staged: null,
    unstaged: null,
    conflict: null,
    submodule: null,
    ...options,
  };
}

function changes(id: string, files: ChangedFile[]): RepositoryChanges {
  return {
    repositoryId: "repository-test" as RepositoryChanges["repositoryId"],
    changeSetId: id as RepositoryChanges["changeSetId"],
    head: {
      oid: "0123456789abcdef",
      branch: "main",
      detached: false,
      upstream: null,
      ahead: null,
      behind: null,
    },
    summary: { staged: 0, unstaged: 0, untracked: 0, conflicted: 0, clean: false },
    files,
  };
}

const snapshotSummary = { staged: 8, unstaged: 7, untracked: 6, conflicted: 5, clean: false };

function summary(staged: number, unstaged: number, untracked: number, conflicted: number) {
  return { staged, unstaged, untracked, conflicted, clean: staged + unstaged + untracked + conflicted === 0 };
}

function diff(changeSetId: string, fileId: string, side: "staged" | "unstaged"): FileDiff {
  return {
    changeSetId: changeSetId as FileDiff["changeSetId"],
    fileId: fileId as FileDiff["fileId"],
    side,
    content: { state: "binary" },
  };
}

function receipt(operation: MutationReceipt["operation"], outcome: MutationReceipt["outcome"], repositoryChanges?: RepositoryChanges): MutationReceipt {
  return { operation, outcome, repositoryChanges, refreshRequired: false, headChanged: false };
}

describe("changes state", () => {
  it("groups mixed staged and unstaged files once, with both selectable sides", () => {
    const mixed = file("mixed", {
      staged: { kind: "modified", oldMode: "100644", newMode: "100644", similarity: null },
      unstaged: { kind: "modified", oldMode: "100644", newMode: "100644", similarity: null },
    });
    const groups = groupChanges([
      mixed,
      file("untracked", {
        unstaged: { kind: "untracked", oldMode: null, newMode: "100644", similarity: null },
      }),
      file("conflict", { conflict: "bothModified" }),
    ]);

    expect(groups.map((group) => [group.id, group.files.map((entry) => entry.fileId)])).toEqual([
      ["conflicted", ["conflict"]],
      ["staged", ["mixed"]],
      ["untracked", ["untracked"]],
    ]);
    expect(availableDiffSides(mixed)).toEqual(["staged", "unstaged"]);
  });

  it("accepts only the newest successful refresh and clears obsolete file handles", () => {
    const first = changes("change-set-one", [file("first")]);
    const second = changes("change-set-two", [file("second")]);
    let state = completeChangesRefresh(beginChangesRefresh(createEmptyChangesState(), 1), 1, first);
    state = beginDiffLoad(state, 1, { fileId: first.files[0].fileId, side: "unstaged" });
    state = beginChangesRefresh(state, 2);

    expect(completeChangesRefresh(state, 1, first)).toBe(state);
    const completed = completeChangesRefresh(state, 2, second);
    expect(completed.data?.changeSetId).toBe(second.changeSetId);
    expect(completed.selected).toBeNull();
    expect(completed.diff.data).toBeNull();
  });

  it("uses the accepted detailed status summary when it differs from the repository snapshot", () => {
    const detailed = changes("change-set-one", [file("first")]);
    detailed.summary = summary(1, 2, 3, 4);
    const state = completeChangesRefresh(beginChangesRefresh(createEmptyChangesState(), 1), 1, detailed);

    expect(workingTreeForChanges(state, snapshotSummary)).toEqual(detailed.summary);
  });

  it("keeps the prior valid list when its newest refresh fails", () => {
    const existing = changes("change-set-one", [file("first")]);
    const state = completeChangesRefresh(beginChangesRefresh(createEmptyChangesState(), 1), 1, existing);
    const failed = failChangesRefresh(beginChangesRefresh(state, 2), 2, error);

    expect(failed.data).toBe(existing);
    expect(failed.error).toBe(error);
    expect(workingTreeForChanges(failed, snapshotSummary)).toEqual(existing.summary);
  });

  it("does not let a stale refresh replace the accepted summary", () => {
    const accepted = changes("change-set-one", [file("first")]);
    accepted.summary = summary(2, 0, 0, 0);
    const newer = changes("change-set-two", [file("second")]);
    newer.summary = summary(0, 3, 0, 0);
    let state = completeChangesRefresh(beginChangesRefresh(createEmptyChangesState(), 1), 1, accepted);
    state = beginChangesRefresh(state, 2);

    expect(completeChangesRefresh(state, 1, accepted)).toBe(state);
    expect(workingTreeForChanges(state, snapshotSummary)).toEqual(accepted.summary);
    state = completeChangesRefresh(state, 2, newer);
    expect(workingTreeForChanges(state, snapshotSummary)).toEqual(newer.summary);
  });

  it("does not let a stale selected-file response overwrite a newer selection", () => {
    const data = changes("change-set-one", [file("first"), file("second")]);
    let state = completeChangesRefresh(beginChangesRefresh(createEmptyChangesState(), 1), 1, data);
    state = beginDiffLoad(state, 1, { fileId: data.files[0].fileId, side: "unstaged" });
    state = beginDiffLoad(state, 2, { fileId: data.files[1].fileId, side: "unstaged" });

    expect(completeDiffLoad(state, 1, diff("change-set-one", "first", "unstaged"))).toBe(state);
    const completed = completeDiffLoad(state, 2, diff("change-set-one", "second", "unstaged"));
    expect(completed.diff.data?.fileId).toBe(data.files[1].fileId);
  });

  it("ignores stale refresh and selected-file failures", () => {
    const data = changes("change-set-one", [file("first"), file("second")]);
    let state = completeChangesRefresh(beginChangesRefresh(createEmptyChangesState(), 1), 1, data);
    state = beginChangesRefresh(state, 2);
    expect(failChangesRefresh(state, 1, error)).toBe(state);

    state = completeChangesRefresh(state, 2, data);
    state = beginDiffLoad(state, 1, { fileId: data.files[0].fileId, side: "unstaged" });
    state = beginDiffLoad(state, 2, { fileId: data.files[1].fileId, side: "unstaged" });
    expect(failDiffLoad(state, 1, error)).toBe(state);
  });

  it("retires selected handles during a file stage and replaces state only from its receipt", () => {
    const oldData = changes("change-set-one", [file("first", { unstaged: { kind: "modified", oldMode: "100644", newMode: "100644", similarity: null } })]);
    const nextData = changes("change-set-two", [file("first", { staged: { kind: "modified", oldMode: "100644", newMode: "100644", similarity: null } })]);
    let state = completeChangesRefresh(beginChangesRefresh(createEmptyChangesState(), 1), 1, oldData);
    state = beginDiffLoad(state, 1, { fileId: oldData.files[0].fileId, side: "unstaged" });
    state = beginMutation(state, 2, "stageFile");

    expect(state.stale).toBe(true);
    expect(state.selected).toBeNull();
    expect(state.diff.data).toBeNull();
    expect(canMutateChanges(state)).toBe(false);
    const completed = completeMutation(state, 2, receipt("stageFile", "applied", nextData));
    expect(completed.data).toBe(nextData);
    expect(completed.stale).toBe(false);
    expect(completed.mutation.feedback?.outcome).toBe("applied");
  });

  it("covers stage-all and unstage-all through the same receipt boundary", () => {
    const data = changes("change-set-one", [file("first")]);
    const stagedData = changes("change-set-two", [file("first")]);
    const unstagedData = changes("change-set-three", [file("first")]);
    let state = completeChangesRefresh(beginChangesRefresh(createEmptyChangesState(), 1), 1, data);
    state = beginMutation(state, 2, "stageAll");
    state = completeMutation(state, 2, receipt("stageAll", "applied", stagedData));
    expect(state.data).toBe(stagedData);
    state = beginMutation(state, 3, "unstageAll");
    state = completeMutation(state, 3, receipt("unstageAll", "applied", unstagedData));
    expect(state.data).toBe(unstagedData);
  });

  it("rejects duplicate, superseded, and repository-switched mutation responses", () => {
    const data = changes("change-set-one", [file("first")]);
    const nextData = changes("change-set-two", [file("first")]);
    let state = completeChangesRefresh(beginChangesRefresh(createEmptyChangesState(), 1), 1, data);
    state = beginMutation(state, 2, "stageFile");
    expect(beginMutation(state, 3, "stageFile")).toBe(state);
    expect(completeMutation(state, 3, receipt("stageFile", "applied", nextData))).toBe(state);

    const switched = createEmptyChangesState();
    expect(completeMutation(switched, 2, receipt("stageFile", "applied", nextData))).toBe(switched);
  });

  it("keeps rejected and uncertain mutations explicit without reauthorizing old handles", () => {
    const data = changes("change-set-one", [file("first")]);
    const refreshed = changes("change-set-two", [file("first")]);
    let state = completeChangesRefresh(beginChangesRefresh(createEmptyChangesState(), 1), 1, data);
    state = beginMutation(state, 2, "unstageFile");
    state = completeMutation(state, 2, { ...receipt("unstageFile", "rejected", refreshed), issue: error });
    expect(state.data).toBe(refreshed);
    expect(state.mutation.feedback?.outcome).toBe("rejected");

    state = beginMutation(state, 3, "stageFile");
    state = completeMutation(state, 3, { ...receipt("stageFile", "uncertain"), refreshRequired: true });
    expect(state.stale).toBe(true);
    expect(state.mutation.feedback?.outcome).toBe("uncertain");
    expect(state.mutation.feedback?.refreshRequired).toBe(true);
  });

  it("marks typed stale failures as non-authoritative until a successful refresh", () => {
    const data = changes("change-set-one", [file("first")]);
    const refreshed = changes("change-set-two", [file("first")]);
    let state = completeChangesRefresh(beginChangesRefresh(createEmptyChangesState(), 1), 1, data);
    state = beginMutation(state, 2, "stageFile");
    state = failMutation(state, 2, "stageFile", error, true);
    expect(state.stale).toBe(true);
    expect(state.mutation.feedback?.outcome).toBe("stale");
    state = completeChangesRefresh(beginChangesRefresh(state, 3), 3, refreshed);
    expect(state.stale).toBe(false);
    expect(state.mutation.feedback?.refreshRequired).toBe(false);
  });

  it("requires a repository and history refresh after commits or observed HEAD movement", () => {
    expect(shouldRefreshAfterMutation(receipt("createCommit", "applied"))).toBe(true);
    expect(shouldRefreshAfterMutation({ ...receipt("stageFile", "applied"), headChanged: true })).toBe(true);
    expect(shouldRefreshAfterMutation({ ...receipt("stageFile", "uncertain"), refreshRequired: true })).toBe(true);
    expect(shouldRefreshAfterMutation(receipt("stageFile", "applied"))).toBe(false);
  });
});
