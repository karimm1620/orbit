# Orbit — Coding Agent Instructions

This file is binding for Codex and other coding agents working in the repository.

The goal is to keep Orbit secure, reviewable, lightweight, and aligned with the product documentation.

---

## 1. Read before editing

Before substantial implementation, read:

1. `docs/PRD.md`
2. `docs/ARCHITECTURE.md`
3. `docs/SECURITY.md`
4. `docs/DECISIONS.md`
5. `docs/ROADMAP.md`
6. `docs/TODO.md`

Do not infer product behavior from generic Git clients when the repository docs already define it.

When documents disagree:

1. `SECURITY.md` wins for security-sensitive behavior.
2. `DECISIONS.md` wins for locked architecture choices.
3. `PRD.md` defines product requirements.
4. `ARCHITECTURE.md` defines technical boundaries.
5. `ROADMAP.md` and `TODO.md` define execution order.

---

## 2. Work milestone-by-milestone

Do not implement future milestone features simply because they are nearby.

For example, during M0 do not add:

- final commit graph rendering
- staging mutations
- diff editor
- GitHub authentication
- SQLite
- global state libraries
- syntax-highlighting frameworks

unless the user explicitly changes the scope.

Prefer a complete vertical slice over a broad partial implementation.

---

## 3. Preserve existing project conventions

If the Tauri scaffold or repository already has a working structure:

- follow it
- make minimal moves
- do not perform speculative directory migrations
- do not rewrite working code just to match a preferred template

Refactors need a concrete benefit.

---

## 4. Security rules are mandatory

Any change involving:

- IPC
- Tauri capabilities
- shell/process execution
- Git commands
- filesystem access
- URLs
- authentication
- credentials
- destructive operations
- native plugins

must be checked against `docs/SECURITY.md`.

Never create:

```text
run_shell(command)
run_git(args)
read_any_file(path)
```

as frontend-accessible commands.

Never invoke Git through `sh -c`, `bash -c`, `zsh -c`, or equivalent command strings for ordinary product behavior.

---

## 5. Native Git is the source of truth

Use the user's native `git` executable.

Do not replace Git semantics with:

- libgit2
- a JavaScript Git reimplementation
- manual `.git` directory parsing

unless a future ADR explicitly changes the decision.

Prefer Git's machine-readable output formats and Git's own validation commands.

---

## 6. Treat all repository data as untrusted

This includes:

- paths
- filenames
- branch names
- remote names
- commit messages
- author data
- diff content
- config values

Do not interpolate repository data into shell strings.

Do not render repository-controlled content as raw HTML.

Use `--` for user-controlled pathspecs where appropriate.

---

## 7. Dependency discipline

Do not add dependencies preemptively.

Before adding one, state:

- the requirement it solves
- why existing runtime/platform capabilities are insufficient
- maintenance/security implications
- relevant size/performance implications

Keep dependencies scoped.

Use pnpm for JavaScript dependencies.

Do not silently switch package managers.

Commit the lockfile.

---

## 8. Testing expectations

New parsing or privileged behavior requires tests.

Prefer:

- Rust unit tests for parsers/validation/error mapping
- temporary-repository integration tests for Git behavior
- frontend tests for UI state/interactions

Tests for Git parsing should include unusual valid filenames where practical.

Do not mock native Git in every test if a temporary real Git repository provides better confidence.

---

## 9. Quality gates

Before declaring work complete, run the checks that exist for the current repository.

Expected categories:

- lint
- TypeScript typecheck
- frontend tests
- formatting
- Rust tests
- `cargo fmt --check`
- `cargo clippy`
- appropriate Tauri build/check command

If a check cannot run because of an environment/toolchain limitation:

- state exactly which check was not run
- state why
- do not claim it passed
- do not invent performance/build evidence

---

## 10. Git workflow

Default workflow:

```text
main
→ feature branch
→ implementation
→ validation
→ PR
→ review
→ squash merge
```

Do not force-push or bypass repository protections unless the user explicitly requests it and doing so is appropriate.

Do not merge before review when the user asked for review first.

---

## 11. Keep commits and PRs focused

Use clear English commit messages.

Examples:

```text
feat: add repository selection
feat: parse porcelain repository status
fix: handle detached HEAD snapshots
test: cover unusual git filenames
```

Avoid vague messages such as:

```text
update
fix stuff
changes
```

PR descriptions should summarize:

- behavior implemented
- architecture impact
- security-sensitive changes
- validation performed
- known limitations
- deliberately deferred work

---

## 12. Update documentation with implementation truth

If implementation changes a locked decision:

- do not silently diverge
- update `DECISIONS.md`
- explain the new evidence/tradeoff

If milestone scope changes:

- update `ROADMAP.md`
- update `TODO.md`

If a new privileged boundary appears:

- update `SECURITY.md`

Documentation should describe what is actually true, not the original plan after reality changes.

---

## 13. Performance claims

Orbit targets low overhead, but do not claim:

- RAM usage
- startup speed
- package size
- graph smoothness

without actual measurement.

When profiling is performed, record:

- environment
- repository/sample
- method
- result

Avoid anecdotal "feels fast" acceptance criteria.

---

## 14. Error behavior

Do not expose raw stack traces as the normal user experience.

Prefer structured errors with:

- stable code
- contextual title/message
- operation
- recoverability
- optional sanitized diagnostic detail

Do not make frontend logic depend on matching arbitrary localized Git error text when a stable category can be derived.

---

## 15. Keep the WebView low privilege

Frontend code should request domain operations.

Example:

```text
switchBranch(repositoryId, branchName)
```

not infrastructure primitives.

When uncertain whether something belongs in React or Rust, privileged local behavior belongs on the Rust side.

---

## 16. No hidden workaround rule

Do not "make the test pass" by:

- disabling security checks
- skipping validation
- weakening types broadly
- ignoring errors
- removing tests
- bypassing hooks/CI silently

Fix the underlying issue or document a genuine blocker.

---

## 17. Completion response format

At the end of a substantial task, report:

### Implemented
What changed.

### Validation
Exact checks/tests run and their results.

### Security
Any IPC/process/filesystem/auth/destructive changes, or "none".

### Known limitations
Anything relevant that remains unsupported or unverified.

### Git state
Branch, commit(s), PR number/status if applicable.

Do not claim actions that were not actually performed.
