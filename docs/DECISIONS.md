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

## ADR-018 — Graph history uses native Git with an opaque frontier cursor

**Status:** Accepted

### Decision

Acquire graph commits with a bounded, NUL-framed `git log --topo-order` query. Acquire local
branches, remote-tracking branches, and tags once per history session with a separate bounded
`git for-each-ref` query.

Pagination is a Rust-owned session containing unresolved full-OID frontier tips and already-emitted
OIDs. React receives only an opaque cursor and cannot supply revision expressions.

### Why

- topological order supplies the parent-after-child invariant needed by the lane reducer
- `for-each-ref` preserves ref kinds, symbolic refs, and annotated-tag peeling without parsing
  decoration text
- frontier paging advances from unfinished history lines instead of repeatedly traversing an
  ever-growing skipped prefix
- a session snapshot prevents ref changes from silently altering the meaning of later pages

### Consequences

- initial pages default to 100 commits, requests above 200 are rejected, active frontier tips are
  capped at 512, and sessions stop at 1,000 loaded commits until the renderer/virtualization gate
  is measured; bound failures are explicit rather than silently truncating graph lines
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
- retaining the starting frontier prevents unrelated repository changes from silently splicing
  different topology into later pages
- repository/root binding preserves the M0 authorization boundary

### Consequences

- cursors are opaque handles, not authorization secrets; the repository ID/root binding is always
  checked as well
- session state intentionally does not survive an application restart
- eviction or expiry is recoverable by starting a fresh history request

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
