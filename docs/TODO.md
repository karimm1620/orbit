# Orbit — Current TODO

**Current milestone:** M1 — Commit Graph
**Rule:** keep this file execution-focused. Move completed historical detail into PR/commit history instead of turning TODO into a permanent journal.

---

## 0. M1 architecture gate

- [x] Preserve the M0 native Git / Rust / typed IPC security boundary.
- [x] Research topological history ordering and machine-readable framing.
- [x] Test native Git acquisition against a real nonlinear temporary repository.
- [x] Decide ref acquisition and annotated-tag peeling.
- [x] Decide opaque incremental cursor/frontier behavior.
- [x] Prototype deterministic lane allocation and page continuation.
- [x] Decide DOM/SVG/Canvas rendering split.
- [x] Decide initial virtualization policy.
- [x] Review history/ref commands for configured process and network execution.
- [x] Record accepted architecture in canonical documentation.

The accepted architecture is in `M1_COMMIT_GRAPH_RESEARCH.md` and ADR-018 through ADR-020.
No product graph UI was implemented during this gate.

---

## 1. Native history service

- [ ] Add a purpose-specific Rust commit-graph history service.
- [ ] Capability-probe `--no-lazy-fetch`; fail closed for graph reads when unsupported.
- [ ] Query bounded `git log --topo-order` pages through the existing `GitRunner`.
- [ ] Parse seven-field NUL-framed commit records.
- [ ] Validate OIDs for the repository object format, parents, timestamps, UTF-8, and bounds.
- [ ] Cover linear, branch/merge, multiple-merge, octopus, detached, and unborn histories.
- [ ] Map missing objects and repository changes to structured errors.

---

## 2. Ref service

- [ ] Query refs once per history session with bounded `git for-each-ref`.
- [ ] Parse local branches, remote-tracking branches, tags, and symbolic refs.
- [ ] Peel annotated tags only when they target commits.
- [ ] Group multiple refs per commit deterministically.
- [ ] Cover Unicode valid ref names and multiple refs on one commit.

---

## 3. Incremental session and typed IPC

- [ ] Add a Rust-owned history session bound to an authorized repository ID.
- [ ] Keep unresolved frontier OIDs and emitted OIDs behind an opaque cursor.
- [ ] Default to 100 commits, clamp requests to 200, cap active frontier tips at 512, and cap the initial session at 1,000.
- [ ] Expire stale history sessions.
- [ ] Restart the ref/history snapshot on explicit refresh.
- [ ] Add purpose-specific `get_commit_graph_page` IPC and matching TypeScript types.
- [ ] Verify React cannot supply paths, Git arguments, or revision expressions.

---

## 4. Topology model

- [ ] Implement the pure TypeScript lane reducer independently of rendering.
- [ ] Preserve stable monotonic lane IDs across pages.
- [ ] Prefer first-parent continuation and deterministic secondary-parent order.
- [ ] Deduplicate converging lanes and compact only visual columns.
- [ ] Emit continuation stubs for parents beyond the current page.
- [ ] Add deterministic fixtures for linear, merge, nested merge, octopus, page split, and parent-outside-window history.

---

## 5. Initial graph renderer

- [ ] Keep commit subject, author, time, refs, selection, and keyboard behavior in semantic DOM.
- [ ] Draw only topology nodes/edges in an `aria-hidden` per-row SVG strip.
- [ ] Add accessible text for HEAD, ref, and merge information conveyed visually.
- [ ] Support empty, loading, incremental-loading, detached, unborn, and error states.
- [ ] Add commit selection and read-only commit details within M1 scope.
- [ ] Keep fixed row geometry compatible with future windowing.
- [ ] Do not add graph, animation, global-state, or virtualization dependencies initially.

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

- [ ] Prove graph reads do not invoke signature verification programs.
- [ ] Prove graph reads do not invoke textconv, external diff, pager, or fsmonitor programs.
- [ ] Prove partial-clone graph reads do not perform lazy network fetches.
- [ ] Preserve `core.fsmonitor=false` for status reads.
- [ ] Confirm no arbitrary frontend Git/filesystem/process capability.
- [ ] Confirm Tauri capabilities remain least-privilege.
- [ ] Render all repository-controlled data as untrusted text.

---

## 8. Validation before M1 PR

- [ ] `pnpm lint`
- [ ] `pnpm typecheck`
- [ ] frontend topology/UI tests
- [ ] `cargo fmt --check`
- [ ] `cargo test --locked`
- [ ] `cargo clippy --locked --all-targets --all-features -- -D warnings`
- [ ] appropriate Tauri check/build validation
- [ ] real-repository desktop smoke covering nonlinear history and load-more
- [ ] final security and dependency review
- [ ] no M2+ mutation or diff implementation in the branch

---

## 9. M0 carryover (not part of commit-graph implementation)

- [ ] Complete a real-repository UI smoke through the native picker when the environment can drive
  the desktop portal; the M0 session launched both app and picker but could not confirm a folder.
- [ ] Implement recent-repository persistence only in a separately scoped, security-reviewed task.
