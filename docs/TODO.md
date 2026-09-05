# Orbit — Current TODO

**Current milestone:** M0 — Repository Foundation  
**Rule:** keep this file execution-focused. Move completed historical detail into PR/commit history instead of turning TODO into a permanent journal.

---

## 0. Before implementation

- [x] Create the Orbit repository.
- [x] Add the canonical bootstrap docs.
- [x] Confirm `git` is installed and record the development Git version.
- [x] Confirm Rust toolchain.
- [x] Confirm pnpm.
- [x] Install the official Linux/Tauri system prerequisites for Arch.
- [x] Scaffold current Tauri 2 + React + TypeScript project using pnpm.
- [x] Run the untouched scaffold before major edits.
- [x] Commit the clean scaffold baseline.
- [x] Create `feat/m0-repository-foundation`.

---

## 1. Project quality baseline

- [x] Preserve the generated Tauri project conventions unless there is a concrete reason to change them.
- [x] Configure TypeScript strictness appropriate for the scaffold.
- [x] Configure lint/format scripts.
- [x] Add Rust formatting/clippy/test commands to documented validation.
- [x] Add a minimal GitHub Actions workflow for Linux validation.
- [x] Keep lockfiles committed.
- [x] Do not add graph/state/database/highlighting libraries in M0.

---

## 2. Rust error model

- [x] Define structured application error type.
- [x] Add stable error codes for M0.
- [x] Map "git executable missing".
- [x] Map invalid/non-repository selection.
- [x] Map repository unavailable.
- [x] Map unsupported Git/repository state where needed.
- [x] Ensure raw stderr is not the only frontend contract.
- [x] Add unit tests for mappings.

---

## 3. Git process boundary

- [x] Create one low-level Git process runner.
- [x] Ensure no shell is used.
- [x] Allow explicit working directory/repository context.
- [x] Capture exit status/stdout/stderr safely.
- [x] Add Git version detection.
- [x] Add size/encoding handling appropriate to each parser.
- [x] Remove inherited `GIT_*` overrides from M0 child processes.
- [x] Prevent frontend access to the generic process runner.
- [x] Add unit/integration tests.

---

## 4. Repository selection and authorization

- [x] Add native directory picker through the privileged side.
- [x] Handle user cancellation.
- [x] Resolve selected location through Git rather than only checking for a `.git` directory.
- [x] Resolve canonical repository/work-tree root.
- [x] Create opaque runtime repository ID/context.
- [x] Return typed repository descriptor.
- [ ] Revalidate recent paths before reopening.
- [x] Verify no broad filesystem capability was accidentally enabled.

---

## 5. Repository status parser

- [x] Use stable machine-readable Git output.
- [x] Parse current branch.
- [x] Parse detached HEAD.
- [x] Parse HEAD OID.
- [x] Parse upstream if configured.
- [x] Parse ahead/behind if available.
- [x] Parse staged entries.
- [x] Parse unstaged entries.
- [x] Parse untracked entries.
- [x] Parse conflicts.
- [x] Prevent status reads from executing configured content-filter programs.
- [x] Test spaces in filenames.
- [x] Test Unicode filenames.
- [x] Test leading `-` filename.
- [x] Test newline-containing filename where the filesystem/Git allows it.
- [x] Test repository with no commits.

---

## 6. Recent commit history

- [x] Define bounded history query.
- [x] Return full OID.
- [x] Return short OID.
- [x] Return parent OIDs.
- [x] Return subject.
- [x] Return author display name.
- [x] Return author email only if the UI requires it.
- [x] Return machine-friendly timestamp.
- [x] Test merge commit parents.
- [x] Test unusual commit text.
- [x] Ensure history size is bounded in M0.

---

## 7. Typed command API

- [x] Add `select_repository`.
- [x] Add `get_repository_snapshot` if refresh is separate.
- [x] Keep IPC commands purpose-specific.
- [x] Add typed TypeScript wrappers.
- [x] Centralize frontend invocation/error conversion.
- [x] Verify no `run_shell`.
- [x] Verify no frontend `run_git(args)`.
- [x] Verify no arbitrary path read/write command.

---

## 8. M0 frontend

- [x] Create no-repository empty state.
- [x] Add Open Repository action.
- [x] Add loading state during repository read.
- [x] Render repository display name/path.
- [x] Render current branch or detached HEAD.
- [x] Render HEAD short OID.
- [x] Render clean/dirty summary.
- [x] Render staged/unstaged/untracked/conflict counts.
- [x] Render recent commit list.
- [x] Render contextual invalid-repository error.
- [x] Provide explicit refresh.
- [x] Do not build the final graph yet.
- [x] Avoid decorative dashboard-card overload.

---

## 9. Recent repositories

Implement only if it remains small and does not distract from the trusted Git path.

Deferred from M0: runtime repository authorization and the trusted Git path take priority over persistent recent-repository metadata.

- [ ] Define minimal persistence format.
- [ ] Store canonical path + display metadata + last opened.
- [ ] Revalidate on reopen.
- [ ] Remove from list without touching disk.
- [ ] Handle missing/moved repository safely.

---

## 10. Integration test repository matrix

- [x] normal repository with commits
- [x] empty repository / unborn branch
- [x] detached HEAD
- [x] modified tracked file
- [x] staged file
- [x] staged + unstaged changes on same file
- [x] untracked file
- [ ] renamed file
- [ ] merge commit
- [x] conflict state where practical
- [x] invalid directory
- [x] unusual filenames

Use temporary repositories created by tests; do not rely only on the developer's real projects.

---

## 11. Validation before PR

- [x] pnpm install uses the committed lockfile.
- [x] frontend lint passes.
- [x] frontend typecheck passes.
- [ ] frontend tests pass if introduced.
- [x] `cargo fmt --check` passes.
- [x] `cargo clippy` passes under the project's agreed policy.
- [x] Rust tests pass.
- [x] Tauri development/check/build validation appropriate to the environment passes.
- [ ] M0 manual smoke test opens at least one real repository.
- [x] Security checklist reviewed.
- [x] No mock repository data remains in accepted path.
- [x] No unsupported performance claims.
- [x] Documentation updated for actual implementation.

Frontend tests were not introduced in M0. The desktop process and native picker launch were
smoke-tested, but the headless automation session could not confirm a folder in the desktop
portal, so the real-repository manual UI smoke remains explicitly unverified.

---

## 12. PR

Suggested title:

```text
feat: establish repository foundation
```

PR body should include:

- implemented vertical slice
- architecture changes
- security-sensitive changes
- tests/checks run
- known limitations
- screenshots only if useful
- follow-up explicitly deferred to M1

Do not merge while required checks or material review findings are unresolved.
