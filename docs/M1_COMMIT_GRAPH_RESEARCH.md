# Orbit M1 — Commit Graph Research

**Status:** Accepted architecture for M1 implementation
**Scope:** read-only bounded commit graph; this document does not authorize M2+ mutations

This document records the experiments and decisions behind Milestone 1. The canonical security
and product rules in `SECURITY.md`, `DECISIONS.md`, and `PRD.md` still take precedence.

## 1. Decision summary

M1 will use:

- native `git log --topo-order` for commit relationships and metadata
- a separate bounded `git for-each-ref` query for typed refs and annotated-tag peeling
- a Rust-owned opaque history session whose cursor contains a frontier of unresolved commit OIDs
- semantic commit/ref pages over purpose-specific typed IPC
- a deterministic pure TypeScript lane reducer that carries continuation state between pages
- normal DOM rows for text and interaction, with a presentation-only SVG topology strip
- no graph, state-management, animation, or virtualization dependency initially

## 2. Git history acquisition

### Command contract

The M1 history service will build a direct, separate-argument invocation equivalent to:

```text
git --no-pager --no-lazy-fetch --no-optional-locks \
  -c core.abbrev=12 \
  -c log.showSignature=false \
  log --topo-order --no-decorate --no-notes --no-patch \
  --no-ext-diff --no-textconv --encoding=UTF-8 \
  --max-count=<rust-validated-limit> \
  --format=%H%x00%h%x00%P%x00%an%x00%at%x00%ct%x00%s \
  -z <rust-owned-frontier-oid>... --
```

This is an argument vector, not a shell command. `GitRunner` remains the only production module
that launches the process.

The seven fixed fields are:

1. full object ID
2. unambiguous abbreviated object ID, with a minimum length of 12
3. space-separated full parent object IDs
4. raw author display name
5. author timestamp as epoch seconds
6. commit timestamp as epoch seconds
7. subject

Records and variable text fields are NUL-framed. The parser must validate field count, UTF-8,
object-ID syntax and algorithm length, parent IDs, numeric timestamps, page count, and the existing
bounded stdout/stderr limits. Full OIDs are authoritative; abbreviated OIDs are display-only.

`%an` is deliberate. `%aN` applies mailmap rules and can consult a configured external
`mailmap.file`, which is not required to render history. The commit timestamp is the primary
display/sort timestamp; author time remains available for details.

### Ordering

`--topo-order` is the graph contract: no parent is emitted before its children, and parallel lines
are kept together where Git can do so. `--date-order` and `--author-date-order` preserve the
parent-after-child constraint but intermix independent lines according to their respective
timestamps. They are useful alternate views, not the initial graph order.

M1 does not parse `git log --graph`, decorations, default pretty formats, localized labels, or
color/ANSI output.

### Git compatibility

`--no-lazy-fetch` was added in Git 2.45. M1 history initialization must capability-probe this
global option rather than parse vendor version strings. If it is unsupported, the graph request
returns a structured, recoverable unsupported-state error explaining that secure graph reads need
a Git version with lazy-fetch suppression. The existing M0 snapshot remains available.

## 3. Ref acquisition

Refs are not parsed from log decorations. Rust will make one bounded query when it creates or
refreshes a history session:

```text
git --no-pager --no-lazy-fetch --no-optional-locks \
  for-each-ref --sort=refname \
  --format=%(objectname)%00%(objecttype)%00%(*objectname)%00%(*objecttype)%00%(refname)%00%(symref) \
  refs/heads refs/remotes refs/tags
```

`for-each-ref` appends one newline per record. Ref names cannot contain control characters, so the
parser uses the fixed six NUL-separated fields and verifies the record newline rather than treating
arbitrary human-formatted decoration text as structure.

Mapping rules:

- `refs/heads/*` becomes a local-branch ref only when its target is a commit.
- `refs/remotes/*` becomes a remote-tracking ref only when its target is a commit.
- A lightweight tag uses `objectname` when `objecttype` is `commit`.
- An annotated tag uses the peeled `*objectname` only when `*objecttype` is `commit`.
- Tags that ultimately target non-commit objects are not attached to graph rows.
- Symbolic remote refs such as `refs/remotes/origin/HEAD` retain their `symref` target and render as
  an alias, not as an indistinguishable duplicate branch.
- Multiple refs may target one commit and are grouped by full target OID in deterministic refname
  order.

HEAD identity continues to come from the repository snapshot/status boundary: attached HEAD has a
branch and OID, detached HEAD has only an OID, and an unborn branch has a name but no commit OID.
Unborn repositories return an empty graph page without error.

The separate ref query is justified because it preserves full names, types, symbolic targets, and
annotated-tag peeling without decoration-string parsing. It runs once per history session, not once
per page.

## 4. Incremental loading

### Why not skip/count

Repeated `--skip=N --max-count=M` queries make Git walk and discard a growing prefix for each page.
That is an obvious route to quadratic cumulative work as the user loads older history. A revision
range based only on the last displayed OID also loses independent lanes that were active at the
page boundary.

### Frontier cursor

Rust owns a process-local history session bound to the opaque repository ID. Its opaque frontend
cursor addresses state containing:

- the ref-tip snapshot used to start traversal
- unresolved frontier OIDs
- the set of OIDs already emitted by this session
- page-size policy and session identity

For each page:

1. run the history command from every unresolved frontier tip
2. discard any already-emitted OID defensively
3. return at most the Rust-validated page size
4. retain input frontier tips that were not emitted
5. add parents of emitted commits that have not been emitted
6. deduplicate the next frontier by full OID while preserving deterministic order

This state is necessary because a bounded page may end while multiple independent histories are
still active. The frontend cannot provide arbitrary revision expressions: it sends only
`repositoryId`, an opaque `cursor`, and an optional requested limit that Rust validates. B1 defaults
to 100 commits per page, accepts 1 through 200 per request, caps the frontier at 512 active tips,
and stops at 1,000 loaded commits per session. Exceeding a bound returns a structured request,
unsupported, or too-large state; it never silently drops history lines. The implemented registry
keeps at most eight process-local sessions, expires them after 15 minutes idle, evicts the oldest
session at the active bound, and rotates a single-use cursor after each successful page. The
1,000-commit ceiling is revisited only after the virtualization/profile gate.

An explicit refresh starts a new session and new ref snapshot. It does not splice changed refs into
an existing traversal. This gives each paging session a coherent view even if Git changes between
requests. Missing objects or removed repositories produce structured errors and allow a restart.

The frontier experiment used a real ten-commit repository with two merges, local and remote refs,
lightweight and annotated tags, detached HEAD, and a Unicode ref. Four pages of three commits
produced the same OID set and order as one bounded topological walk, without duplicates. This is a
correctness experiment, not a large-repository performance claim.

## 5. Typed graph model

The B1 purpose-specific IPC is:

```ts
getCommitHistoryPage(
  repositoryId: RepositoryId,
  cursor?: HistoryCursor | null,
  pageSize?: number,
): Promise<CommitHistoryPage>;
```

The response model is:

```ts
type CommitHistoryPage = {
  commits: GraphCommit[];
  refs: CommitRef[]; // populated on the first page of a session
  head:
    | { state: "attached"; branch: string; oid: string }
    | { state: "detached"; oid: string }
    | { state: "unborn"; branch: string };
  nextCursor: HistoryCursor | null;
  hasMore: boolean;
  sessionLimitReached: boolean;
};

type GraphCommit = {
  oid: string;
  shortOid: string;
  parentOids: string[];
  subject: string;
  authorName: string;
  authorTimestamp: number;
  committedTimestamp: number;
};

type CommitRef = {
  kind:
    | "localBranch"
    | "remoteTrackingBranch"
    | "lightweightTag"
    | "annotatedTag"
    | "symbolicRef";
  fullName: string;
  displayName: string;
  targetOid: string;
  symbolicTarget: string | null;
};
```

Rust supplies semantic Git relationships, not renderer coordinates. The frontend derives rows with
a lane ID, current column, parent edges, and the continuation state after that row. Repository text
continues to render through normal React text nodes.

Selecting a loaded commit sends only the full OID already issued by the active history session to a
separate purpose-specific details command. Row selection can immediately show the fields and refs
already present in the page. Full message and changed-file metadata are separately bounded reads;
they do not enlarge every history row or expose a generic `git show` API. The details command must
apply the same no-pager, no-lazy-fetch, no-signature, no-external-diff, and no-textconv policy before
it is implemented.

## 6. Lane algorithm

The frontend uses a pure deterministic reducer over commits in Git's children-before-parents order.
Reducer state is an ordered list of active lanes:

```ts
type ActiveLane = {
  laneId: number;     // monotonic identity; never reused in a session
  expectedOid: string;
};
```

For each commit:

1. Find lanes whose `expectedOid` equals the commit OID. If none exists, insert a new lane for a
   disconnected ref tip.
2. If several lanes converge on the commit, use the leftmost lane as primary and retire the other
   matching lanes after emitting convergence edges.
3. Continue the first parent in the primary lane. If that parent is already expected by another
   lane, connect to that lane and retire the duplicate instead.
4. Insert secondary-parent lanes immediately to the right of the primary lane, in parent order,
   unless an active lane already expects that parent.
5. A root commit closes its primary lane. Closed visual columns compact, but lane identities are
   never reassigned.

Invariants:

- every displayed commit has one node and every parent relationship has one edge
- a parent never appears before a child
- at most one active lane expects a given OID after each reduction
- first-parent continuation is preferred; secondary-parent order is stable
- lane IDs are stable across appended pages even when compacted column indexes change
- reducing pages while carrying state is identical to reducing the concatenated commit sequence
- unresolved parents at the last row produce explicit continuation stubs
- the result depends only on commits plus prior reducer state, never DOM measurements

The disposable lane prototype exercised linear history, a branch/merge, an octopus merge, a branch
point inside the loaded window, and a parent beyond the window. It verified deterministic one-pass
versus paged reduction and that all lanes close when complete history is supplied. The separate
native-Git experiment covered nested merges, multiple refs on one commit, and detached HEAD.
Implementation will preserve these cases as pure unit-test fixtures.

## 7. Rendering decision

| Option | Strengths | Costs for Orbit | Decision |
| --- | --- | --- | --- |
| DOM only | Native text, focus, selection, testing | Curves and crossings become awkward element/CSS geometry | Reject for graph strokes |
| One SVG | Vector curves and nodes | Couples all rows into one surface; text/focus/list behavior is harder | Reject as the full UI |
| Canvas 2D | Low element count; flexible drawing | Requires DPI scaling, redraw/hit testing, and a parallel accessible DOM | Defer |
| DOM + Canvas | Accessible text plus scalable drawing | Synchronization and testing complexity is unjustified without profiling | Defer |
| DOM + row SVG | Native accessible rows plus simple vector topology | Must align fixed row geometry and continuation edges | **Selected** |

Each fixed-height commit row is a normal selectable DOM row containing normal text and ref labels.
A narrow SVG strip draws only the node and edge segments for that row. The SVG is
`aria-hidden="true"` because the adjacent DOM exposes equivalent semantic text such as merge parent
count, HEAD state, and refs. Keyboard focus, selection state, context, and actions remain on DOM
controls; topology color is supplemented by shape/line treatment rather than used as the only cue.

Per-row SVG keeps pagination appendable and leaves a path to fixed-row windowing. SVG's vector
coordinate system handles high-DPI scaling without a canvas backing-store protocol. A canvas
renderer may be reconsidered only from measured renderer cost, not visual novelty.

## 8. Virtualization policy

M1 does not initially need a virtualization dependency. The first view renders one 100-row page;
additional pages are explicit and bounded. Before M1 acceptance, profile 100, 500, and 1,000 loaded
rows on a recorded Linux environment and nonlinear repository fixture.

Use three repeatable scroll and selection runs in a release-mode desktop build. Fixed-row windowing
is required if, at 1,000 rows on the recorded primary Linux hardware, any repeat shows either a
95th-percentile scroll frame interval above 16.7 ms or a 95th-percentile selection-to-next-paint
latency above 100 ms. Also introduce it if profiling identifies DOM/layout work as the dominant
cause of a visible interaction stall, even when timer instrumentation is unavailable. Record the
environment, repository, method, raw result, and profiler trace before choosing either a small
library or an internal window. Until that gate, do not claim large-repository performance and do
not enable unbounded DOM accumulation.

Fixed row height, renderer independence, stable lane IDs, and continuation state are requirements
so virtualization can be added without changing Git acquisition.

## 9. Security review

History and refs are read-only in product semantics, but Git configuration can still introduce
process or network behavior. M1 commands therefore:

- use only the existing internal `GitRunner`, direct executable invocation, closed stdin, bounded
  output, and the Rust-authorized repository root
- use a purpose-specific graph IPC; no `run_git`, shell, arbitrary path, or revision expression is
  exposed to React
- use `--no-pager`, so pager programs cannot run
- override `log.showSignature=false`, so repository config cannot cause signature verification via
  GPG/SSH helpers
- specify `--no-patch --no-ext-diff --no-textconv`, so external diff and textconv drivers are not
  part of a history read
- use `--no-notes` and an explicit format, avoiding configured notes/default pretty formats
- use `--no-lazy-fetch` plus `GIT_NO_LAZY_FETCH=1`, so missing partial-clone objects do not trigger
  a fetch, credential helper, remote helper, or network access
- use `--no-optional-locks`, preserving read-only behavior where Git has optional maintenance/index
  refresh paths
- retain the M0 environment scrub and treat all returned text as untrusted

Git aliases do not replace built-in command names, and neither selected command normally executes
repository hooks. Future commit-detail or diff commands require a separate threat review; these
decisions do not pre-authorize textconv, filters, signature verification, network access, or hooks.

The M0 recent-history query is hardened in this research change with explicit no-signature,
no-decoration, no-notes, no-patch, no-external-diff, and no-textconv options. `GitRunner` restores
`GIT_NO_LAZY_FETCH=1` after removing inherited `GIT_*` variables. M1 additionally capability-probes
and requires the explicit Git 2.45+ global option for its graph command.

## 10. Dependencies

No dependency is selected or installed in this phase.

- Generic DAG layout libraries model arbitrary graphs and do not naturally preserve Git's
  first-parent/topological conventions or incremental frontier state.
- SVG and DOM are already available in the WebView.
- The lane reducer is small, deterministic, Git-specific, and independently testable.
- React state is sufficient for a single repository history session during M1.
- A virtualization dependency remains conditional on measured need and a separate maintenance,
  bundle-size, accessibility, and compatibility review.

## 11. Implementation handoff

M1 B1 completed the Rust history-session state, structured parsers, ref mapping, capability probe,
purpose-specific typed IPC, and temporary real-repository integration fixtures. B2a completed the
pure deterministic topology reducer and pagination fixtures. B2b should proceed in this order:

1. consume `get_commit_history_page` from the M1 graph state layer
2. implement the accessible DOM-row + SVG-strip renderer and incremental loading states
3. profile the defined row counts before deciding whether windowing is needed

Do not add commit mutation, branch mutation, diff rendering, provider APIs, persistence, file
watching, or final visual polish as part of that slice.

## 12. Primary references

- [Git log ordering, formatting, and process-sensitive options](https://git-scm.com/docs/git-log)
- [Git for-each-ref format atoms and ref metadata](https://git-scm.com/docs/git-for-each-ref)
- [Git global no-lazy-fetch and no-optional-locks options](https://git-scm.com/docs/git)
- [Git partial clone missing-object behavior](https://git-scm.com/docs/partial-clone)
- [Git 2.45 release notes for no-lazy-fetch](https://github.com/git/git/blob/master/Documentation/RelNotes/2.45.0.adoc)
- [W3C SVG accessibility support](https://www.w3.org/TR/SVG/access)
- [MDN Canvas accessible fallback requirements](https://developer.mozilla.org/en-US/docs/Web/API/Canvas_API/Tutorial/Basic_usage)
