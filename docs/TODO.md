# Orbit — Current TODO

**Current milestone:** M2 — Changes & Diff

**Rule:** keep this file execution-focused. Move completed historical detail into PR/commit history instead of turning TODO into a permanent journal.

---

## 0. M2 architecture gate

- [x] Preserve the M0/M1 native Git, Rust authorization, typed IPC, and low-privilege WebView boundary.
- [x] Verify porcelain v2 detailed status framing in disposable repositories.
- [x] Verify staged, unstaged, both-sided, rename/copy, conflict, unborn, binary, and untracked Git behavior.
- [x] Verify spaces, Unicode, tabs, newlines, leading dashes, and non-UTF-8 path bytes.
- [x] Decide opaque change-set/file identity and safe path display.
- [x] Decide staged, unstaged, and untracked diff acquisition.
- [x] Decide Rust patch-parser placement and typed non-text states.
- [x] Test filter, textconv, external-diff, fsmonitor, special-file, and lazy-fetch threat surfaces.
- [x] Lock conservative output, memory, registry, and command-duration bounds.
- [x] Define explicit refresh and stale-response semantics.
- [x] Record the accepted design in `M2_CHANGES_DIFF_RESEARCH.md` and ADR-022 through ADR-024.

---

## 1. Detailed changes service

- [x] Extend the existing status parser to return byte-safe semantic entries and derive M0 counts from them.
- [x] Parse ordinary, rename/copy, unmerged, and untracked porcelain v2 NUL records.
- [x] Represent staged and unstaged facets independently on one file entry.
- [x] Preserve raw current/origin paths only in Rust and expose deterministic escaped display text.
- [x] Add bounded opaque change-set/file handles tied to an authorized repository/root.
- [x] Add purpose-specific `get_repository_changes` IPC and matching TypeScript types.
- [x] Apply explicit rename/copy and submodule policies from the research decision.
- [x] Add parser bounds and structured malformed/expired/replaced-handle errors.

---

## 2. Diff acquisition and parser

- [ ] Extend `GitRunner` with a bounded M2 deadline without adding another process boundary.
- [ ] Add hardened staged and unstaged selected-file diff operations.
- [ ] Add the bounded Linux no-index path for untracked regular files and symlinks.
- [ ] Revalidate change-set/file/side identity and worktree file type before acquisition.
- [ ] Parse and validate the combined NUL numstat prefix plus patch transition.
- [ ] Classify binary output from numstat rather than patch prose.
- [ ] Parse bounded unified hunks and lines in pure Rust without trusting patch-header paths.
- [ ] Return typed text, binary, conflict, submodule, too-large, unsupported-encoding, timeout, stale, and unavailable states.
- [ ] Keep combined conflict patches, binary previews, and non-UTF-8 decoding out of the initial viewer.

---

## 3. Changes and diff UI

- [ ] Integrate detailed changes with the existing repository refresh generation.
- [ ] Present staged, unstaged, untracked, and conflicted state without duplicating one logical entry unnecessarily.
- [ ] Preserve staged-plus-unstaged state on the same file.
- [ ] Add accessible file selection and selected-side behavior.
- [ ] Render typed hunks as ordinary escaped DOM text.
- [ ] Add explicit binary, conflict, submodule, too-large, unavailable, loading, and empty states.
- [ ] Keep refresh and file-switch errors contextual without discarding the prior valid list.
- [ ] Reject stale change-set/diff responses after refresh or repository switch.
- [ ] Do not add syntax highlighting until its need and cost are measured.

---

## 4. M2 tests and security gate

- [ ] Cover every supported porcelain record/XY form, bounds, malformed output, and raw path cases.
- [ ] Cover unified hunk parsing, line accounting, metadata-only changes, no-newline markers, invalid UTF-8, and bounds.
- [ ] Cover real staged, unstaged, both-sided, add/delete, rename/copy, unborn, untracked, conflict, binary, symlink, and special-file repositories.
- [ ] Prove M2 reads do not execute content filters, textconv, external diff, pager, fsmonitor, hooks, or promisor remotes.
- [ ] Prove special-file/replacement races cannot leave a Git child running indefinitely.
- [ ] Cover successful refresh replacement, failed-refresh preservation, stale file handles, and stale frontend responses.
- [ ] Confirm no generic Git/process/filesystem IPC and no Tauri capability expansion.
- [ ] Run the complete frontend and Rust gate plus the Tauri debug build before the M2 PR.
- [ ] Perform a real-repository desktop smoke when the environment can drive the native picker.

---

## 5. Carryover evidence gaps

These remain visible and are not retroactively completed by starting M2:

- [ ] Profile 100, 500, and 1,000 commit rows on a recorded Linux environment, including the documented p95 scroll-frame and selection-to-paint thresholds across three runs.
- [ ] Decide whether fixed-row windowing is needed from that evidence; do not claim large-repository performance before measurement.
- [ ] Complete a real-repository M1 desktop smoke covering nonlinear history and load-more.
- [ ] Complete the M0 native-picker-to-real-repository smoke when the environment can drive the desktop portal.
- [ ] Implement recent-repository persistence only in a separately scoped, security-reviewed task.
