import { describe, expect, it } from "vitest";

import {
  availableDiffSides,
  beginChangesRefresh,
  beginDiffLoad,
  completeChangesRefresh,
  completeDiffLoad,
  createEmptyChangesState,
  failChangesRefresh,
  groupChanges,
  workingTreeForChanges,
} from "./changesState";
import type { ChangedFile, FileDiff, OrbitError, RepositoryChanges } from "./tauri";

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
});
