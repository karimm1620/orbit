# Orbit — Architecture Decision Record

**Status:** Active baseline

This document records decisions that should not be repeatedly reopened without new evidence.

Each decision can be revised later, but a revision must explain what changed and why.

---

## ADR-001 — Use Tauri 2.x for the desktop shell

**Status:** Accepted

### Decision

Orbit's initial desktop shell uses Tauri 2.x with a React + TypeScript frontend and Rust privileged layer.

### Why

- small native-oriented desktop architecture
- Rust boundary fits Git/process work
- Linux support
- explicit capability model
- lower-overhead direction than shipping a full browser runtime as the application architecture

### Consequences

- native Linux system dependencies are required for development
- privileged behavior belongs in Rust/Tauri commands
- Tauri capability changes are security-sensitive
- claims about resource usage must still be measured rather than assumed

---

## ADR-002 — Linux-first, Arch as a first-class development environment

**Status:** Accepted

### Decision

Linux is the first supported/validated desktop platform.

Arch Linux is explicitly supported as a development environment.

### Consequences

- early CI and manual validation prioritize Linux
- platform abstractions should not block future Windows/macOS work
- future platform support is added after Linux behavior is stable

---

## ADR-003 — Use the user's native Git executable

**Status:** Accepted

### Decision

Orbit uses the installed `git` executable as the Git engine.

Do not start with libgit2, `git2-rs`, JGit-like reimplementations, or a custom Git engine.

### Why

- native Git is the behavioral reference users already trust
- it respects existing Git config and authentication
- feature compatibility is broad
- it reduces semantic divergence from terminal workflows
- Orbit is an interface for Git, not a replacement implementation

### Consequences

- Orbit requires a compatible Git executable
- Git version detection is needed
- output must be parsed using machine-readable formats
- subprocess execution becomes a critical security boundary

---

## ADR-004 — No generic shell or Git command over IPC

**Status:** Accepted and security-critical

### Decision

The frontend receives only purpose-specific commands.

Rejected APIs:

```text
run_shell(command)
run_git(args)
read_any_file(path)
```

### Consequences

- more commands may need to be modeled explicitly
- the privileged boundary remains auditable
- frontend compromise does not automatically become arbitrary shell access

See `SECURITY.md`.

---

## ADR-005 — Rust owns repository selection and authorization

**Status:** Accepted

### Decision

Repository selection/validation is performed through the privileged side.

The opened repository is represented by a runtime repository context/identifier.

### Why

This keeps filesystem authority and canonical repository identity out of generic frontend APIs.

### Consequences

- recent paths are revalidated on reopen
- repository-scoped commands should move toward repository IDs rather than arbitrary absolute paths

---

## ADR-006 — No backend service for core Orbit

**Status:** Accepted

### Decision

The core desktop Git experience has no Orbit backend dependency.

### Consequences

- local repository features work offline
- no Orbit account is required
- remote Git uses Git's own transport/auth mechanisms
- provider APIs remain optional integrations

---

## ADR-007 — No database in Milestone 0

**Status:** Accepted

### Decision

Do not add SQLite or another database during M0.

### Why

Initial persistent state is small:

- recent repositories
- preferences

A database can be introduced when a real query/data-lifecycle need appears.

---

## ADR-008 — No global frontend state library by default

**Status:** Accepted

### Decision

Start with React's built-in state mechanisms and feature-local abstractions.

### Consequences

A library such as a query cache/store can be added only after a concrete cross-feature state problem appears.

---

## ADR-009 — Machine-readable Git output only where practical

**Status:** Accepted

### Decision

Prefer programmatic formats such as porcelain/NUL-delimited data over parsing output intended for people.

### Consequences

- parsers must have fixture/unit tests
- unusual filenames are first-class test cases
- UI strings must not depend on localized Git output for normal success paths

---

## ADR-010 — Incremental history loading

**Status:** Accepted

### Decision

Commit history is bounded/incremental.

Orbit does not load the entire repository graph before showing the workspace.

### Why

- large repositories
- startup latency
- memory use
- lower-end hardware

---

## ADR-011 — Graph model is separate from graph renderer

**Status:** Accepted

### Decision

Git history parsing, lane/topology calculation, list virtualization, and rendering are separate concerns.

### Consequences

The renderer can evolve without rewriting Git acquisition.

---

## ADR-012 — Existing Git credentials remain authoritative

**Status:** Accepted

### Decision

Basic fetch/pull/push use existing Git authentication and configuration.

Orbit does not implement a custom password/SSH-key vault for ordinary Git transport.

### Consequences

Provider API auth is treated separately when needed.

---

## ADR-013 — Dark mode at initial product stage

**Status:** Accepted

### Decision

Dark mode is supported from the beginning.

Light mode is not required for M0.

### Consequences

Tokens should not hard-code a design that makes future light mode impossible.

---

## ADR-014 — Dependency additions are milestone-driven

**Status:** Accepted

### Decision

Do not preinstall libraries for:

- graph rendering
- global state
- syntax highlighting
- animation
- database
- hosting providers
- file watching

until the active milestone demonstrates a need.

### Why

Orbit targets a small, understandable dependency surface and low overhead.

---

## ADR-015 — PR-based milestone workflow

**Status:** Accepted

### Decision

Substantial work uses:

```text
main
→ feature branch
→ implementation
→ validation
→ pull request
→ review
→ squash merge
```

### Consequences

- each milestone should remain reviewable
- docs and tests travel with the implementation
- failed CI is fixed rather than bypassed by default

---

## ADR-016 — Performance claims require evidence

**Status:** Accepted

### Decision

Do not claim:

- low RAM use
- fast startup
- small binary/package size
- smooth large-repository behavior

without measurement from the relevant environment.

### Consequences

Early documentation describes goals, not fabricated benchmarks.

---

## ADR-017 — Security rules override convenience

**Status:** Accepted and security-critical

### Decision

If a simpler implementation conflicts with `SECURITY.md`, choose the safer boundary even if it requires more typed commands or Rust work.

---

## ADR-018 — Graph history uses native Git with an opaque ordered-plan cursor

**Status:** Accepted

### Decision

Acquire one bounded full-OID order with `git log --topo-order` at session start, capped at the
1,000-commit session ceiling plus one truncation sentinel. Acquire each page's seven-field,
NUL-framed metadata for the next Rust-owned OID slice with `git log --no-walk=unsorted`. Acquire
local branches, remote-tracking branches, and tags once per session with a separate bounded
`git for-each-ref` query.

Pagination is a Rust-owned session containing the bounded ordered OID plan and its next index.
React receives only an opaque cursor and cannot supply revision expressions.

### Why

- topological order supplies the parent-after-child invariant needed by the lane reducer
- `for-each-ref` preserves ref kinds, symbolic refs, and annotated-tag peeling without parsing
  decoration text
- a single bounded topological walk preserves Git's traversal-queue decisions across every page;
  reconstructing a walk from incomparable frontier tips was proven page-boundary-sensitive
- per-page metadata reads avoid repeatedly traversing an ever-growing skipped prefix
- a session snapshot prevents ref changes from silently altering the meaning of later pages

### Consequences

- initial pages default to 100 commits, requests above 200 are rejected, starting tips are capped
  at 512, and sessions stop at 1,000 loaded commits until the renderer/virtualization gate is
  measured; the ordered plan is bounded to 1,001 OIDs and reports the sentinel as an explicit limit
- explicit refresh creates a new history session
- the M1 history capability requires Git's `--no-lazy-fetch` support; unsupported Git versions
  retain the M0 snapshot but receive a structured graph error
- exact command and model details live in `M1_COMMIT_GRAPH_RESEARCH.md`

---

## ADR-019 — Git topology is a pure frontend model rendered with DOM and SVG

**Status:** Accepted

### Decision

Rust returns semantic commits, parent OIDs, refs, and cursor state. A deterministic pure TypeScript
reducer assigns stable lane identities and per-row edge geometry while carrying continuation state
between pages.

Render each commit as an accessible fixed-height DOM row. Use a narrow, presentation-only SVG in
that row for graph nodes and edges.

### Why

- renderer coordinates are not Git domain data
- normal DOM retains text selection, focus, keyboard behavior, semantics, and straightforward tests
- SVG handles merge curves and high-DPI scaling without Canvas redraw, hit-testing, or duplicate
  accessibility infrastructure
- per-row geometry supports incremental append and future fixed-row windowing

### Consequences

- visual topology must not be the only accessible source of HEAD, refs, or merge information
- lane IDs remain stable across pages even when visual columns compact
- Canvas remains an evidence-driven fallback, not an initial dependency

---

## ADR-020 — Defer commit-list virtualization until profiling

**Status:** Accepted

### Decision

Do not add a virtualization dependency for the initial 100-row graph page. Profile 100, 500, and
1,000 loaded rows on a recorded Linux environment before M1 acceptance.

Require fixed-row windowing if any of three release-mode runs at 1,000 rows on the recorded primary
Linux hardware shows a 95th-percentile scroll frame interval above 16.7 ms or a 95th-percentile
selection-to-next-paint latency above 100 ms. A profiler-confirmed DOM/layout interaction stall is
also sufficient evidence. Review a library against a small internal fixed-row window only after
that evidence exists.

### Consequences

- no large-repository performance claim is made by this decision
- fixed row height and renderer/model separation are required now so windowing can be added later
- unbounded DOM accumulation is not approved

---

## ADR-021 — History sessions use bounded process-local rotating cursors

**Status:** Accepted

### Decision

Expose `get_commit_history_page(repository_id, cursor, page_size)` as the sole B1 history IPC.
Omitting the cursor starts a session and snapshots semantic HEAD, typed refs, and their commit tips.
Each successful continuation consumes its cursor and issues a new opaque cursor.
Before page I/O, the registry marks the cursor in flight. Concurrent reuse fails. A capability or
Git page-read failure restores the same cursor; successful page completion atomically removes it
and, when more buffered history remains, inserts the rotated cursor.

Keep at most eight active sessions, expire sessions after 15 minutes idle, and evict the least
recently accessed session when inserting beyond the active bound. Page size defaults to 100 and
must be between 1 and 200; invalid values are rejected with a structured error. A session stops at
1,000 emitted commits and reports whether unresolved history remains.

Bind every session to its authorized repository ID and canonical root. A process restart,
unknown/expired/reused cursor, repository mismatch, unavailable repository, or changed root
identity invalidates continuation. Later HEAD/ref movement does not alter the captured traversal;
explicit refresh starts a new snapshot. Typed refs are emitted only on the first page.

### Why

- rotating single-use cursors make invalid state transitions deterministic without exposing Git
  revision expressions
- bounded process-local state needs no database and cannot grow without a fixed ceiling
- retaining the bounded ordered OID plan prevents unrelated repository changes from silently
  splicing different topology into later pages
- explicit available/in-flight state preserves successful single-use semantics without consuming
  a cursor on a failed page
- repository/root binding preserves the M0 authorization boundary

### Consequences

- cursors are opaque handles, not authorization secrets; the repository ID/root binding is always
  checked as well
- session state intentionally does not survive an application restart
- eviction or expiry is recoverable by starting a fresh history request
- in-flight sessions are not selected for active-session eviction

---

## ADR-022 — Working-tree identity uses porcelain v2 and opaque change handles

**Status:** Accepted

### Decision

Acquire M2's detailed working-tree state with one hardened
`git status --porcelain=v2 --branch --untracked-files=all -z` query. Parse path fields as bytes in
Rust, model staged and unstaged facets independently, and derive summary counts from the same
semantic result.

Install each successful result as a bounded process-local change set. React identifies a selected
entry only with the authorized repository ID, opaque change-set ID, and opaque file ID. Byte-exact
current/origin paths remain inside Rust; IPC exposes only safe display text.

### Why

- porcelain v2 represents ordinary, rename/copy, unmerged, and untracked state without parsing
  localized display output
- NUL framing preserves spaces, Unicode, tabs, newlines, leading dashes, and non-UTF-8 path bytes
- a single semantic parse prevents detailed entries and M0 counts from disagreeing
- opaque handles preserve filesystem authority on the privileged side

### Consequences

- status output and retained entries/path bytes have explicit limits
- successfully refreshing a repository invalidates its previous change set and selected diff
- Git's output order is undefined, so Rust applies deterministic bytewise path ordering
- Orbit reports Git's rename/copy result and does not guess relationships for an unstaged move
- exact framing, limits, and display escaping are recorded in `M2_CHANGES_DIFF_RESEARCH.md`

---

## ADR-023 — File diffs are purpose-specific Rust-parsed Git responses

**Status:** Accepted

### Decision

Acquire a selected staged facet with `git diff --cached`, an unstaged tracked facet with
`git diff`, and an untracked Linux file with a bounded `git diff --no-index` comparison against
`/dev/null`. Rust supplies every path after `--`; React supplies no path or revision.

Request a NUL-framed numstat prefix and unified patch in one Git invocation. Validate that the
numstat identity exactly matches the Rust-held entry, use `-\t-` for binary classification, and
parse text hunks in Rust. Do not use patch-header filenames as identity or send raw patch output to
React.

For rename/copy facets, pass the Rust-held origin and target together and constrain the resulting
record with the corresponding closed `--diff-filter=R` or `--diff-filter=C`. This preserves Git's
relationship detection without allowing a changed copy source to become an unexpected second file
in the selected response.

Return binary, conflict, submodule, too-large, unsupported-encoding, special-file, timeout, stale,
and unavailable results as typed states. Combined conflict patches, binary previews, and lossy
text decoding are not part of initial M2.

### Why

- staged and unstaged facets compare different Git states and can coexist on one file
- one command avoids classifying one filesystem version and parsing another
- numstat is machine-readable for binary detection while patch prose is not
- Rust parsing centralizes bounds, malformed-output handling, and untrusted path separation
- no-index is the native-Git way to represent an untracked file as an addition

### Consequences

- all M2 reads reuse `GitRunner`, require the secure no-lazy-fetch capability, and neutralize
  pager, optional locks, fsmonitor, content filters, textconv, and external diff execution
- configured filter drivers containing `=` fail closed before status or diff execution because
  Git's `-c name=value` grammar cannot encode the exact dynamic key safely
- text patch content must be UTF-8 initially; other encodings get an explicit unavailable state
- untracked paths receive a non-following type check, and M2 Git reads receive a 30-second process
  deadline because a FIFO experiment proved that output bounds alone cannot prevent a stalled read
- no new production dependency is approved

---

## ADR-024 — Change sets use read-committed refresh semantics

**Status:** Accepted

### Decision

A successful detailed-status refresh creates a new change set and retires the prior set for that
repository. A failed refresh leaves the prior displayed result available with a contextual error.
Before diff acquisition, Rust reauthorizes the repository/root, validates the change-set/file/side
tuple, and confirms that a fresh hardened status record is compatible with the stored facet.

The selected Git invocation reads current content. If another process changes the file without
changing its status classification during that invocation, Orbit does not claim snapshot
isolation. Responses carry their change-set identity, and the frontend request-generation guard
prevents older results from replacing a newer refresh or selection.

### Why

- creating an immutable worktree snapshot would add copying, storage, and lifecycle complexity
  disproportionate to a read-only desktop diff viewer
- cheap semantic revalidation catches deleted, renamed, resolved, or otherwise incompatible state
- retaining prior UI data on refresh failure keeps errors contextual without authorizing stale
  handles indefinitely

### Consequences

- stale handles return a structured refresh-required error
- process-local change sets are bounded, expire, and do not survive restart
- file watching and polling remain deferred; explicit refresh is authoritative in initial M2

---

## Deferred decisions

The following are intentionally not locked yet:

- file-watching library
- syntax highlighter
- state/query library
- packaging formats
- updater strategy
- GitHub auth mechanism
- provider abstraction
- hunk/line staging strategy
- built-in conflict editor architecture

Resolve these only when their milestone begins.
