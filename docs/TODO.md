# Orbit — Current TODO

**Current milestone:** M0 — Repository Foundation  
**Rule:** keep this file execution-focused. Move completed historical detail into PR/commit history instead of turning TODO into a permanent journal.

---

## 0. Before implementation

- [ ] Create the Orbit repository.
- [ ] Add the canonical bootstrap docs.
- [ ] Confirm `git` is installed and record the development Git version.
- [ ] Confirm Rust toolchain.
- [ ] Confirm pnpm.
- [ ] Install the official Linux/Tauri system prerequisites for Arch.
- [ ] Scaffold current Tauri 2 + React + TypeScript project using pnpm.
- [ ] Run the untouched scaffold before major edits.
- [ ] Commit the clean scaffold baseline.
- [ ] Create `feat/m0-repository-foundation`.

---

## 1. Project quality baseline

- [ ] Preserve the generated Tauri project conventions unless there is a concrete reason to change them.
- [ ] Configure TypeScript strictness appropriate for the scaffold.
- [ ] Configure lint/format scripts.
- [ ] Add Rust formatting/clippy/test commands to documented validation.
- [ ] Add a minimal GitHub Actions workflow for Linux validation.
- [ ] Keep lockfiles committed.
- [ ] Do not add graph/state/database/highlighting libraries in M0.

---

## 2. Rust error model

- [ ] Define structured application error type.
- [ ] Add stable error codes for M0.
- [ ] Map "git executable missing".
- [ ] Map invalid/non-repository selection.
- [ ] Map repository unavailable.
- [ ] Map unsupported Git/repository state where needed.
- [ ] Ensure raw stderr is not the only frontend contract.
- [ ] Add unit tests for mappings.

---

## 3. Git process boundary

- [ ] Create one low-level Git process runner.
- [ ] Ensure no shell is used.
- [ ] Allow explicit working directory/repository context.
- [ ] Capture exit status/stdout/stderr safely.
- [ ] Add Git version detection.
- [ ] Add size/encoding handling appropriate to each parser.
- [ ] Prevent frontend access to the generic process runner.
- [ ] Add unit/integration tests.

---

## 4. Repository selection and authorization

- [ ] Add native directory picker through the privileged side.
- [ ] Handle user cancellation.
- [ ] Resolve selected location through Git rather than only checking for a `.git` directory.
- [ ] Resolve canonical repository/work-tree root.
- [ ] Create opaque runtime repository ID/context.
- [ ] Return typed repository descriptor.
- [ ] Revalidate recent paths before reopening.
- [ ] Verify no broad filesystem capability was accidentally enabled.

---

## 5. Repository status parser

- [ ] Use stable machine-readable Git output.
- [ ] Parse current branch.
- [ ] Parse detached HEAD.
- [ ] Parse HEAD OID.
- [ ] Parse upstream if configured.
- [ ] Parse ahead/behind if available.
- [ ] Parse staged entries.
- [ ] Parse unstaged entries.
- [ ] Parse untracked entries.
- [ ] Parse conflicts.
- [ ] Test spaces in filenames.
- [ ] Test Unicode filenames.
- [ ] Test leading `-` filename.
- [ ] Test newline-containing filename where the filesystem/Git allows it.
- [ ] Test repository with no commits.

---

## 6. Recent commit history

- [ ] Define bounded history query.
- [ ] Return full OID.
- [ ] Return short OID.
- [ ] Return parent OIDs.
- [ ] Return subject.
- [ ] Return author display name.
- [ ] Return author email only if the UI requires it.
- [ ] Return machine-friendly timestamp.
- [ ] Test merge commit parents.
- [ ] Test unusual commit text.
- [ ] Ensure history size is bounded in M0.

---

## 7. Typed command API

- [ ] Add `select_repository`.
- [ ] Add `get_repository_snapshot` if refresh is separate.
- [ ] Keep IPC commands purpose-specific.
- [ ] Add typed TypeScript wrappers.
- [ ] Centralize frontend invocation/error conversion.
- [ ] Verify no `run_shell`.
- [ ] Verify no frontend `run_git(args)`.
- [ ] Verify no arbitrary path read/write command.

---

## 8. M0 frontend

- [ ] Create no-repository empty state.
- [ ] Add Open Repository action.
- [ ] Add loading state during repository read.
- [ ] Render repository display name/path.
- [ ] Render current branch or detached HEAD.
- [ ] Render HEAD short OID.
- [ ] Render clean/dirty summary.
- [ ] Render staged/unstaged/untracked/conflict counts.
- [ ] Render recent commit list.
- [ ] Render contextual invalid-repository error.
- [ ] Provide explicit refresh.
- [ ] Do not build the final graph yet.
- [ ] Avoid decorative dashboard-card overload.

---

## 9. Recent repositories

Implement only if it remains small and does not distract from the trusted Git path.

- [ ] Define minimal persistence format.
- [ ] Store canonical path + display metadata + last opened.
- [ ] Revalidate on reopen.
- [ ] Remove from list without touching disk.
- [ ] Handle missing/moved repository safely.

---

## 10. Integration test repository matrix

- [ ] normal repository with commits
- [ ] empty repository / unborn branch
- [ ] detached HEAD
- [ ] modified tracked file
- [ ] staged file
- [ ] staged + unstaged changes on same file
- [ ] untracked file
- [ ] renamed file
- [ ] merge commit
- [ ] conflict state where practical
- [ ] invalid directory
- [ ] unusual filenames

Use temporary repositories created by tests; do not rely only on the developer's real projects.

---

## 11. Validation before PR

- [ ] pnpm install uses the committed lockfile.
- [ ] frontend lint passes.
- [ ] frontend typecheck passes.
- [ ] frontend tests pass if introduced.
- [ ] `cargo fmt --check` passes.
- [ ] `cargo clippy` passes under the project's agreed policy.
- [ ] Rust tests pass.
- [ ] Tauri development/check/build validation appropriate to the environment passes.
- [ ] M0 manual smoke test opens at least one real repository.
- [ ] Security checklist reviewed.
- [ ] No mock repository data remains in accepted path.
- [ ] No unsupported performance claims.
- [ ] Documentation updated for actual implementation.

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
