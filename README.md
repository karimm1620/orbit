# Orbit

Orbit is a fast, visual, local-first desktop Git client designed around the way Git actually works.

The initial product is Linux-first and developer-focused. Orbit does not replace Git, hide Git concepts behind proprietary abstractions, or require a cloud account for its core experience. Git remains the source of truth; Orbit provides a clearer interface for understanding and controlling repositories.

## Product direction

Orbit is built around a few non-negotiable ideas:

- **Local-first:** core workflows work without an Orbit account or backend.
- **Git remains Git:** branches, commits, HEAD, staging, remotes, conflicts, merge, and rebase remain visible concepts.
- **Visual-first:** commit history, repository state, diffs, and relationships should be easier to understand at a glance.
- **Keyboard-friendly:** common workflows should eventually be accessible through shortcuts and a command palette.
- **Safe by default:** destructive operations require explicit, contextual confirmation.
- **Lightweight:** performance is a product requirement, including on lower-end Linux hardware.
- **Secure boundaries:** the WebView never receives arbitrary shell or filesystem execution capability.

## Initial technology direction

The initial architecture uses:

- Tauri 2.x
- React
- TypeScript
- Rust
- pnpm
- the user's native `git` executable

Orbit intentionally starts without a backend service, account system, database, Git abstraction library, or GitHub SDK.

The exact dependency set is locked incrementally as milestones require it.

## Documentation

Read these documents before implementing product behavior:

- [`docs/PRD.md`](docs/PRD.md) — product requirements and scope
- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — system boundaries and technical architecture
- [`docs/SECURITY.md`](docs/SECURITY.md) — mandatory security rules
- [`docs/DECISIONS.md`](docs/DECISIONS.md) — architectural decisions
- [`docs/ROADMAP.md`](docs/ROADMAP.md) — milestone sequence and acceptance outcomes
- [`docs/TODO.md`](docs/TODO.md) — current execution checklist
- [`AGENTS.md`](AGENTS.md) — binding rules for Codex and other coding agents

When documents disagree, use this precedence:

1. `docs/SECURITY.md` for security-sensitive behavior
2. `docs/DECISIONS.md` for locked architectural decisions
3. `docs/PRD.md` for product requirements
4. `docs/ARCHITECTURE.md` for implementation boundaries
5. `docs/ROADMAP.md` and `docs/TODO.md` for execution sequencing

## First milestone

Milestone 0 proves the complete repository-read path:

```text
React UI
   ↓ typed IPC
Tauri command
   ↓
Rust Git boundary
   ↓
native git
   ↓
typed repository snapshot
   ↓
React UI
```

The first usable slice should allow a user to choose a local Git repository and see its real branch, HEAD, working-tree state, and recent commit history.

No mock repository data should remain in the milestone acceptance path.

## Development workflow

Orbit uses a branch and pull-request workflow:

```text
main
  ↓
feature branch
  ↓
implementation
  ↓
tests / lint / typecheck / cargo checks
  ↓
PR
  ↓
review
  ↓
squash merge
```

Do not bypass failed validation simply to complete a milestone.
