# Orbit — Current TODO

**Current milestone:** M3 — Staging & Commit

**Rule:** keep this file execution-focused. Move completed historical detail into PR/commit history instead of turning TODO into a permanent journal.

---

## 0. M3 architecture gate

- [x] Preserve the M0–M2 repository authorization, native Git, typed IPC, and low-privilege WebView boundary.
- [x] Verify byte-safe literal stage-file behavior for tracked, untracked, deleted, type-changed, symlink, mixed, and unusual paths.
- [x] Verify born and unborn stage/unstage file/all behavior with Git 2.55.
- [x] Define stage-all and unstage-all semantics without frontend pathspec or revision authority.
- [x] Map staging filter, fsmonitor, and `post-index-change` execution surfaces.
- [x] Verify bounded stdin commit messages, cleanup behavior, editor/template bypass, and Git's `COMMIT_EDITMSG` behavior.
- [x] Verify commit hooks, hook rejection/message modification, post-commit behavior, and configured signing execution.
- [x] Define normal-commit eligibility for conflicts and in-progress merge/rebase/cherry-pick/revert/am state.
- [x] Prove direct-child timeout is insufficient and lock process-group termination plus applied/rejected/uncertain outcomes.
- [x] Define per-repository mutation serialization, change-handle invalidation, and post-mutation refresh.
- [x] Record the accepted design in `M3_STAGING_COMMIT_RESEARCH.md` and ADR-025 through ADR-027.

---

## 1. M3-B1 — Staging and unstaging service

- [x] Add a bounded Rust-owned mutation registry with one in-flight operation per repository.
- [x] Extend `GitRunner` with opt-in process-group mutation execution, TERM/KILL cleanup, null stdin, and bounded outcomes.
- [x] Fence older change refreshes and retire authorizing handles immediately before a mutation starts.
- [x] Add the fixed conflict/in-progress-operation guard shared by every M3 mutation.
- [x] Implement stage-file with fresh facet/path revalidation and global literal-pathspec mode.
- [x] Implement stage-all with fixed whole-worktree semantics and `add.ignoreErrors=false`.
- [x] Implement born/unborn unstage-file and unstage-all command branches.
- [x] Preserve configured filters and `post-index-change` while neutralizing fsmonitor and lazy fetch.
- [x] Return typed applied/rejected/uncertain receipts with replacement detailed changes when available.
- [x] Add purpose-specific IPC and TypeScript types without UI controls.
- [x] Cover unusual bytes, rename/copy endpoints, mixed facets, stale handles, filters/hooks, partial-failure config, concurrency, deadlines, and lock diagnostics.

---

## 2. M3-B2 — Normal commit service

- [ ] Add bounded stdin writing and strict NUL/blank/64-KiB message validation without trimming accepted text.
- [ ] Reuse the fixed operation-state guard and add staged-content commit eligibility.
- [ ] Require fresh staged content and reject unresolved conflicts or unsupported continuation state.
- [ ] Implement index-only normal commit without editor, amend, paths, `-a`, empty commit, or hook bypass.
- [ ] Preserve and test pre-commit, prepare-commit-msg, commit-msg, post-commit, and configured signing behavior.
- [ ] Compare pre/post HEAD, invalidate stale history, and distinguish completed commit from uncertain process outcome.
- [ ] Cover unborn and detached commits, hook message changes/rejections, signing failure, output bounds, timeout, stale state, and external index races.
- [ ] Add purpose-specific commit IPC and matching TypeScript wrapper.

---

## 3. M3-C — Staging and commit UI

- [ ] Add stage/unstage file controls without exposing paths or optimistic authority.
- [ ] Add stage-all/unstage-all with explicit busy and contextual failure states.
- [ ] Add accessible bounded commit-message input and staged summary.
- [ ] Render applied, rejected, uncertain, stale, and refresh-required mutation feedback.
- [ ] Replace change/history state only from current-generation mutation receipts and refreshes.
- [ ] Preserve keyboard flow, focus visibility, repository switching, and duplicate-activation protection.

---

## 4. M3-D — Final test, security, and polish gate

- [ ] Re-audit every mutation argument, stdin/environment/config surface, and opaque-handle authorization path.
- [ ] Prove no shell, generic Git/process/filesystem IPC, frontend path/revision authority, or Tauri capability expansion.
- [ ] Verify mutation leases, stale/superseded reads, post-state installation, history invalidation, and external lock failures.
- [ ] Verify descendants are stopped and Git is reaped on filter/hook/signing timeout; never auto-delete locks.
- [ ] Run the complete frontend/Rust/Tauri validation gate and report actual counts.
- [ ] Perform a representative real-repository desktop stage/unstage/commit smoke when the environment can drive the native picker.

---

## 5. Carryover evidence gaps

These remain visible and are not retroactively completed by starting M3:

- [ ] Perform the M2 real-repository desktop smoke when the environment can drive the native picker.
- [ ] Profile 100, 500, and 1,000 commit rows on a recorded Linux environment, including the documented p95 scroll-frame and selection-to-paint thresholds across three runs.
- [ ] Decide whether fixed-row windowing is needed from that evidence; do not claim large-repository performance before measurement.
- [ ] Complete a real-repository M1 desktop smoke covering nonlinear history and load-more.
- [ ] Complete the M0 native-picker-to-real-repository smoke when the environment can drive the desktop portal.
- [ ] Implement recent-repository persistence only in a separately scoped, security-reviewed task.
