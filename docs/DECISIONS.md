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

## Deferred decisions

The following are intentionally not locked yet:

- graph renderer: DOM/SVG/canvas/custom
- virtualization library
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
