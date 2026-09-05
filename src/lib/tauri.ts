import { invoke } from "@tauri-apps/api/core";

declare const repositoryIdBrand: unique symbol;
export type RepositoryId = string & { readonly [repositoryIdBrand]: true };

declare const historyCursorBrand: unique symbol;
export type HistoryCursor = string & { readonly [historyCursorBrand]: true };

export type RepositorySnapshot = {
  repositoryId: RepositoryId;
  root: string;
  displayName: string;
  head: {
    oid: string | null;
    branch: string | null;
    detached: boolean;
    upstream: string | null;
    ahead: number | null;
    behind: number | null;
  };
  workingTree: {
    staged: number;
    unstaged: number;
    untracked: number;
    conflicted: number;
    clean: boolean;
  };
  recentCommits: Array<{
    oid: string;
    shortOid: string;
    parents: string[];
    subject: string;
    authorName: string;
    timestamp: number;
  }>;
};

export type OrbitError = {
  code: string;
  title: string;
  message: string;
  operation: string;
  recoverable: boolean;
  details?: string;
};

export type GraphCommit = {
  oid: string;
  shortOid: string;
  parentOids: string[];
  subject: string;
  authorName: string;
  authorTimestamp: number;
  committedTimestamp: number;
};

export type CommitRefKind =
  | "localBranch"
  | "remoteTrackingBranch"
  | "lightweightTag"
  | "annotatedTag"
  | "symbolicRef";

export type CommitRef = {
  kind: CommitRefKind;
  fullName: string;
  displayName: string;
  targetOid: string;
  symbolicTarget: string | null;
};

export type CommitHistoryHead =
  | { state: "attached"; branch: string; oid: string }
  | { state: "detached"; oid: string }
  | { state: "unborn"; branch: string };

export type CommitHistoryPage = {
  commits: GraphCommit[];
  refs: CommitRef[];
  head: CommitHistoryHead;
  nextCursor: HistoryCursor | null;
  hasMore: boolean;
  sessionLimitReached: boolean;
};

export function selectRepository(): Promise<RepositorySnapshot | null> {
  return invoke<RepositorySnapshot | null>("select_repository");
}

export function getRepositorySnapshot(
  repositoryId: RepositoryId,
): Promise<RepositorySnapshot> {
  return invoke<RepositorySnapshot>("get_repository_snapshot", {
    repositoryId,
  });
}

export function getCommitHistoryPage(
  repositoryId: RepositoryId,
  cursor: HistoryCursor | null = null,
  pageSize?: number,
): Promise<CommitHistoryPage> {
  return invoke<CommitHistoryPage>("get_commit_history_page", {
    repositoryId,
    cursor,
    pageSize,
  });
}

export function toOrbitError(error: unknown): OrbitError {
  if (isOrbitError(error)) return error;

  return {
    code: "internal_error",
    title: "Orbit could not complete the request",
    message: "An unexpected error crossed the application boundary.",
    operation: "frontend_request",
    recoverable: true,
  };
}

function isOrbitError(value: unknown): value is OrbitError {
  if (typeof value !== "object" || value === null) return false;

  const candidate = value as Partial<OrbitError>;
  return (
    typeof candidate.code === "string" &&
    typeof candidate.title === "string" &&
    typeof candidate.message === "string" &&
    typeof candidate.operation === "string" &&
    typeof candidate.recoverable === "boolean" &&
    (candidate.details === undefined || typeof candidate.details === "string")
  );
}
