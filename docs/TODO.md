# Orbit — Current TODO

**Current milestone:** M1 — Commit Graph
**Rule:** keep this file execution-focused. Move completed historical detail into PR/commit history instead of turning TODO into a permanent journal.

---

## 0. M1 architecture gate

- [x] Preserve the M0 native Git / Rust / typed IPC security boundary.
- [x] Research topological history ordering and machine-readable framing.
- [x] Test native Git acquisition against a real nonlinear temporary repository.
- [x] Decide ref acquisition and annotated-tag peeling.
- [x] Decide opaque bounded topological-plan cursor behavior.
- [x] Prototype deterministic lane allocation and page continuation.
- [x] Decide DOM/SVG/Canvas rendering split.
- [x] Decide initial virtualization policy.
- [x] Review history/ref commands for configured process and network execution.
- [x] Record accepted architecture in canonical documentation.

The accepted architecture is in `M1_COMMIT_GRAPH_RESEARCH.md` and ADR-018 through ADR-021.
The B2b graph workspace now consumes the bounded history service without adding mutation workflows.

---

## 1. Native history service

- [x] Add a purpose-specific Rust commit-graph history service.
- [x] Capability-probe `--no-lazy-fetch`; fail closed for graph reads when unsupported.
- [x] Capture one bounded `git log --topo-order` OID plan and read page metadata through the existing `GitRunner`.
- [x] Parse seven-field NUL-framed commit records.
- [x] Validate OIDs for the repository object format, parents, timestamps, UTF-8, and bounds.
- [x] Cover linear, branch/merge, multiple-merge, octopus, detached, and unborn histories.
- [x] Map missing objects and repository changes to structured errors.

---

## 2. Ref service

- [x] Query refs once per history session with bounded `git for-each-ref`.
- [x] Parse local branches, remote-tracking branches, tags, and symbolic refs.
- [x] Peel annotated tags only when they target commits.
- [x] Preserve multiple refs per commit in deterministic refname order.
- [x] Cover Unicode valid ref names and multiple refs on one commit.

---

## 3. Incremental session and typed IPC

- [x] Add a Rust-owned history session bound to an authorized repository ID.
- [x] Keep the bounded ordered OID plan and next index behind an opaque cursor.
- [x] Default to 100 commits, reject requests above 200, cap starting tips at 512, and cap the session at 1,000.
- [x] Mark cursors in flight during page reads; restore on failure and rotate only after success.
- [x] Expire stale history sessions and cap the process at eight active sessions.
- [x] Restart the ref/history snapshot on explicit refresh.
- [x] Add purpose-specific `get_commit_history_page` IPC and matching TypeScript types.
- [x] Verify React cannot supply paths, Git arguments, or revision expressions.

---

## 4. Topology model

- [x] Implement the pure TypeScript lane reducer independently of rendering.
- [x] Preserve stable monotonic lane IDs across pages.
- [x] Prefer first-parent continuation and deterministic secondary-parent order.
- [x] Deduplicate converging lanes and compact only visual columns.
- [x] Emit continuation stubs for parents beyond the current page.
- [x] Add deterministic fixtures for linear, merge, nested merge, octopus, page split, and parent-outside-window history.

---

## 5. Initial graph renderer

- [x] Keep commit subject, author, time, refs, selection, and keyboard behavior in semantic DOM.
- [x] Draw only topology nodes/edges in an `aria-hidden` per-row SVG strip.
- [x] Add accessible text for HEAD, ref, and merge information conveyed visually.
- [x] Support empty, loading, incremental-loading, detached, unborn, and error states.
- [x] Add commit selection and read-only commit details within M1 scope.
- [x] Keep fixed row geometry compatible with future windowing.
- [x] Keep unresolved continuation lanes visible through the loaded page boundary.
- [x] Do not add graph, animation, global-state, or virtualization dependencies initially.

---

## 6. Profiling and virtualization gate

- [ ] Profile 100, 500, and 1,000 loaded rows on a recorded Linux environment.
- [ ] Record repository fixture, hardware/software environment, method, and results.
- [ ] Measure the documented p95 scroll-frame and selection-to-paint thresholds across three runs.
- [ ] Decide from evidence whether fixed-row windowing is required before M1 acceptance.
- [ ] If required, compare a small internal window with a narrowly reviewed dependency.
- [ ] Do not claim large-repository performance before measurement.

---

## 7. Security review

- [x] Prove graph reads do not invoke signature verification programs.
- [x] Prove graph reads do not invoke textconv, external diff, pager, or fsmonitor programs.
- [x] Prove partial-clone graph reads do not perform lazy network fetches.
- [x] Preserve `core.fsmonitor=false` for status reads.
- [x] Confirm no arbitrary frontend Git/filesystem/process capability.
- [x] Confirm Tauri capabilities remain least-privilege.
- [x] Render all repository-controlled data as untrusted text.

---

## 8. Validation before M1 PR

- [x] `pnpm install --frozen-lockfile`
- [x] `pnpm lint`
- [x] `pnpm typecheck`
- [x] `pnpm build`
- [x] frontend topology tests (11 Vitest tests)
- [x] frontend graph-layout continuation test (1 Vitest test)
- [x] `cargo fmt --check`
- [x] `cargo check --locked`
- [x] `cargo test --locked` (52 Rust tests)
- [x] `cargo clippy --locked --all-targets --all-features -- -D warnings`
- [x] `pnpm tauri build --debug --no-bundle`
- [ ] real-repository desktop smoke covering nonlinear history and load-more
- [x] final security and dependency review
- [x] no M2+ mutation or diff implementation in the branch

---

## 9. M0 carryover (not part of commit-graph implementation)

- [ ] Complete a real-repository UI smoke through the native picker when the environment can drive
  the desktop portal; the M0 session launched both app and picker but could not confirm a folder.
- [ ] Implement recent-repository persistence only in a separately scoped, security-reviewed task.
