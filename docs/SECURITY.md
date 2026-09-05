# Orbit — Security Model

**Status:** Binding security contract  
**Scope:** all Orbit code, tooling, IPC, Git execution, local filesystem behavior, credentials, and destructive operations

If another document conflicts with this file on a security-sensitive point, this file wins.

---

## 1. Threat model summary

Orbit is a desktop application with access to sensitive local developer resources.

Potentially sensitive assets include:

- local repositories
- uncommitted source code
- repository history
- Git configuration
- remote URLs
- credentials managed by Git/OS tooling
- arbitrary local paths if a boundary is implemented incorrectly

The React WebView must be treated as a lower-privilege layer.

User-controlled repository content must also be treated as untrusted input.

Examples of untrusted data:

- filenames
- directory names
- branch names
- tags
- remote names
- commit messages
- author names/emails
- Git config values
- diff content
- submodule metadata
- remote output

A malicious repository must not gain arbitrary command execution simply because Orbit opens it.

---

## 2. Core security invariants

The following rules are non-negotiable.

### S-001 — No arbitrary shell API

Never expose a frontend command such as:

```text
run_shell(command: string)
```

### S-002 — No generic frontend Git executor

Never expose:

```text
run_git(args: string[])
```

Even without a shell, a generic executor gives the WebView far more capability than the product requires.

### S-003 — No arbitrary filesystem API

The frontend must not receive a general-purpose read/write/delete filesystem capability for the user's machine.

### S-004 — No shell interpolation

Ordinary Git operations must never be implemented with:

```text
sh -c
bash -c
zsh -c
```

or an equivalent command-string shell boundary.

### S-005 — Purpose-specific commands

Expose narrow operations such as:

```text
get_repository_snapshot(repository_id)
stage_file(repository_id, file_id)
switch_branch(repository_id, branch_name)
create_commit(repository_id, message)
```

### S-006 — Repository scope

A command that mutates repository state must operate only on a repository previously authorized/opened by the user and tracked by the Rust side.

### S-007 — Validate at the privileged boundary

Frontend validation improves UX but is not a security boundary.

Rust must validate security-relevant input.

### S-008 — No plaintext credentials

Orbit must not persist:

- passwords
- private keys
- provider access tokens
- refresh tokens

in plaintext configuration files.

### S-009 — No hidden repository upload

Repository content must never be uploaded to Orbit-controlled infrastructure as part of the core product.

### S-010 — Destructive operations need stronger UX

Destructive operations require explicit context and confirmation proportionate to the risk.

---

## 3. Privilege layers

```text
Layer 0 — untrusted repository/user-controlled data
                  ↓
Layer 1 — React WebView
                  ↓ narrow typed IPC
Layer 2 — Tauri command boundary
                  ↓ validated domain request
Layer 3 — Rust repository/Git services
                  ↓ controlled process invocation
Layer 4 — native Git + approved local resources
                  ↓
Layer 5 — user repository / remote
```

Each layer must reduce capability rather than simply forwarding arbitrary input.

---

## 4. Native repository selection

Repository selection should be performed through an approved native dialog path.

Preferred behavior:

1. React invokes `select_repository()`.
2. Rust opens the native directory picker.
3. Rust validates the selected path.
4. Rust resolves the actual Git work-tree root.
5. Rust registers an opaque repository context.
6. React receives a typed repository descriptor/snapshot.

Avoid granting generic filesystem plugin permissions simply to support repository selection.

Any Tauri capability added for dialogs, filesystem, shell, process, URL opening, or other privileged APIs must be reviewed narrowly.

### M0 implementation

The M0 boundary exposes only `select_repository` and `get_repository_snapshot` to the WebView.

- `select_repository` invokes the official dialog plugin from Rust; no dialog or filesystem plugin permission is granted to frontend code.
- Successful selection creates a process-local opaque repository ID mapped to the canonical Git work-tree root in Rust.
- Refresh accepts only that repository ID and revalidates the stored root before reading it.
- The internal Git runner launches `git` directly with separate arguments, closed stdin, bounded stdout/stderr readers, and no shell. It removes inherited `GIT_*` environment variables so launch-time repository/config overrides cannot redirect the authorized repository context.
- After scrubbing inherited Git variables, the runner sets its own `GIT_NO_LAZY_FETCH=1` policy so Git versions that support it do not contact a promisor remote to satisfy a nominally local read.
- Before a status read, Orbit obtains configured filter-driver names through bounded Git config output and overrides each driver's `clean`/`process` command to an empty value with `required=false`. Status also overrides `core.fsmonitor=false`, so opening a repository cannot invoke a configured content-filter program or filesystem-monitor hook.
- Recent-history reads explicitly disable signature verification, decorations, notes, patch output, external diffs, and text conversion. This prevents repository configuration such as `log.showSignature` or a diff/textconv driver from turning the M0 history query into process execution.
- The unused scaffold opener plugin and permission are removed.

### M1 read-only graph boundary

M1 adds only the proposed purpose-specific `get_commit_graph_page` domain operation. It accepts an
opaque authorized repository ID, an opaque Rust-issued cursor, and a Rust-clamped page size. It does
not accept paths, Git argument arrays, ref expressions, or arbitrary object expressions from React.

Graph acquisition remains inside the existing `GitRunner`. The history command must explicitly use:

- `--no-pager`
- `--no-lazy-fetch`
- `--no-optional-locks`
- `-c log.showSignature=false`
- `--no-decorate --no-notes --no-patch --no-ext-diff --no-textconv`

`--no-lazy-fetch` is capability-probed because it was added in Git 2.45. If it is unavailable, M1
graph acquisition fails closed with a structured unsupported-state error rather than risking an
implicit network request, credential-helper invocation, remote helper, or fetch hook in a partial
clone. M0 repository status remains usable.

Refs come from a separate bounded `for-each-ref` call under the same no-pager, no-lazy-fetch,
no-optional-locks process policy. Both parsers validate fixed framing and treat all commit/ref text
as untrusted. SVG graph marks are presentation-only; repository-controlled text remains ordinary
escaped React text and is never interpreted as HTML, a URL, or executable content.

---

## 5. Repository authorization

An opened repository should receive an opaque identifier.

Conceptually:

```text
RepositoryId -> canonical repository root
```

Frontend mutation requests should prefer the identifier over sending arbitrary absolute paths.

On reopening a persisted recent repository:

- canonicalize again
- validate again
- verify that the repository still exists
- create a new authorized runtime context

Do not trust a stored path forever.

---

## 6. Path handling

Paths are untrusted.

Rules:

- use platform path types, not manual string concatenation
- canonicalize only when semantics require it
- be careful with symlinks and worktrees
- do not assume `.git` is always a directory
- use Git to resolve repository/work-tree identity
- never concatenate a path into shell command text
- use `--` before pathspec arguments when applicable
- correctly support spaces, Unicode, newlines, leading `-`, and other unusual valid filenames

Never "sanitize" filenames by silently changing them before passing them back to Git.

The goal is correct argument separation and repository scoping, not destructive normalization.

---

## 7. Git argument safety

Direct process invocation prevents shell injection but does not eliminate option confusion.

For file-oriented commands:

```text
git <operation> [known options] -- <pathspec>
```

should be preferred when the Git subcommand supports it.

Do not let a filename beginning with `-` become a Git option.

For refs:

- validate with Git's own ref validation where appropriate
- do not build a partial home-grown ref grammar unless there is a documented product reason

For object IDs:

- parse/verify according to the operation's needs
- avoid blindly accepting arbitrary revision expressions where a full object ID is expected

---

## 8. Commit messages

Commit messages are arbitrary user text.

They must not be interpolated into shell commands.

Prefer a mechanism that treats message content as data, for example stdin/file input to Git where appropriate.

Do not log full commit messages to telemetry.

If diagnostic logs include messages locally, make that behavior deliberate and documented.

---

## 9. Environment variables

Git behavior can be influenced by environment variables.

Orbit should not blindly inherit or add dangerous process configuration.

Rules:

- do not inject secrets into logs
- do not print the complete process environment
- set environment variables only when required for a documented operation
- treat repository-provided hooks/config behavior as part of the native Git threat surface

M0 read commands deliberately remove inherited `GIT_*` variables at the child-process
boundary. This prevents variables such as `GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`,
and command-line config injection variables from changing repository identity or read behavior.
The runner then adds only its internal `GIT_NO_LAZY_FETCH=1` policy; a caller-provided value cannot
override it.
Future authenticated/mutating operations must explicitly review which Git environment inputs, if
any, need to be restored rather than inheriting them implicitly.

If Orbit ever needs to disable or alter hooks for a specific operation, that must be an explicit architectural decision rather than an accidental side effect.

---

## 10. Git hooks

Opening a repository for read operations should not execute repository hooks.

Content filters are a separate Git-configured process surface from hooks. M0 status reads
neutralize configured `filter.<driver>.clean` and `filter.<driver>.process` commands before
asking Git for working-tree state.

Mutating Git operations may naturally cause hooks depending on Git behavior.

Orbit must not silently bypass user hooks merely for convenience.

When a hook rejects an operation:

- preserve the failure
- provide useful output
- do not automatically retry with hooks disabled

Any future "skip hooks" feature must be explicit, visible, and scoped.

---

## 11. Process output

Git stdout/stderr is untrusted text.

Do not:

- render process output as HTML
- interpret ANSI escape sequences in normal UI surfaces without a safe renderer
- assume UTF-8 for every possible byte stream unless the command/output contract guarantees it
- expose raw output as executable content

Structured parsers should enforce size limits where an unexpectedly large response could create memory pressure.

---

## 12. Frontend rendering

Repository strings can contain malicious-looking content.

React text rendering should remain text rendering.

Avoid:

- `dangerouslySetInnerHTML` for repository-controlled content
- unsanitized HTML/Markdown rendering of commit messages
- constructing executable links from untrusted repository strings
- using repository strings as raw CSS/DOM fragments

If rich Markdown rendering is added later, it requires a dedicated security review.

---

## 13. URL handling

External URLs are privileged actions.

Before opening a URL:

- use an allowlisted scheme, normally `https`
- parse the URL structurally
- show or otherwise preserve user intent
- do not allow arbitrary `file:`, `javascript:`, custom executable schemes, or shell-style strings

Repository remote URLs are data; they are not automatically safe links to open.

---

## 14. Credentials

### Native Git operations

Delegate to existing Git authentication mechanisms.

Orbit should not request or store SSH private keys.

### Provider API integrations

If GitHub or another provider is added:

- use the provider's supported authorization flow
- store long-lived secrets only through platform-appropriate secure storage
- request minimum scopes
- make disconnect/revoke behavior clear
- never commit credentials to repository files
- redact secrets from logs and crash reports

Provider authentication must receive its own milestone security review.

---

## 15. Tauri permissions and capabilities

Use least privilege.

Every capability/plugin permission must have a product requirement.

Review especially:

- shell/process
- filesystem
- dialog
- opener/URL
- clipboard
- updater
- global shortcuts

Do not enable broad default permissions because a plugin was installed.

A dependency being official does not mean every capability it exposes should be enabled.

---

## 16. Destructive operation classes

### Class A — read-only

Examples:

- status
- history
- branch list
- diff

No destructive confirmation is needed.

### Class B — normal mutation

Examples:

- stage
- unstage
- commit
- create branch
- normal checkout

Require clear operation state and errors, but not necessarily a modal confirmation.

### Class C — potentially destructive

Examples:

- discard file changes
- delete unmerged branch
- reset operations
- clean untracked files
- force push
- operations that overwrite local work

Require explicit, contextual confirmation.

The confirmation should identify the target and consequence.

Avoid generic:

```text
Are you sure?
```

Prefer:

```text
Discard all unstaged changes in src/app.tsx?

This will replace the working copy with the version from the index and cannot be undone by Orbit.
```

The exact wording should reflect actual Git semantics.

---

## 17. Force operations

Force operations deserve separate product treatment.

For push, prefer safer Git mechanisms where product semantics allow, such as lease-based force behavior instead of unconditional force.

Do not silently transform a normal user action into a force operation.

Do not offer destructive flags merely because Git supports them.

---

## 18. Files outside the repository

Git can reference paths and repositories in complex ways, including worktrees and submodules.

Orbit must not assume that every relevant file is physically nested under one simple root.

At the same time, generic filesystem browsing must not emerge accidentally from these cases.

Support for:

- submodules
- linked worktrees
- bare repositories
- nested repositories

should be explicit and tested.

If a repository mode is unsupported in a milestone, fail clearly rather than applying unsafe path assumptions.

---

## 19. Symlinks

Do not follow symlinks for arbitrary file operations without considering repository semantics.

When displaying or diffing Git-controlled content, prefer Git as the authority instead of manually dereferencing paths where possible.

Any direct filesystem mutation through a symlink requires careful boundary validation.

---

## 20. Temporary files

If Git operations require temporary files:

- create them in an app-controlled temporary location
- use restrictive/default-safe permissions
- generate unpredictable names using standard temp-file APIs
- clean up when practical
- never store long-lived credentials in them

---

## 21. Logging and diagnostics

Logs must redact:

- provider tokens
- passwords
- auth headers
- SSH key material
- credential-helper output that contains secrets

Avoid logging:

- full environment
- complete repository file contents
- full diffs by default

A sanitized operation log may include:

- operation name
- repository identifier
- Git exit code
- duration
- structured error category

Debug detail must remain local unless the user explicitly chooses to share it.

---

## 22. Telemetry

Core Orbit requires no telemetry.

If telemetry is proposed later:

- it must be documented before implementation
- it must not collect repository content
- it must not collect secrets
- privacy behavior must be clear
- the product must remain usable without telemetry

---

## 23. Supply-chain security

Dependency additions must be deliberate.

For each new dependency:

- verify the canonical package/repository
- inspect maintenance activity
- understand native/build-script behavior
- avoid unnecessary dependencies
- keep lockfiles committed
- use CI dependency/security checks where practical

Do not execute untrusted installation scripts outside the standard package/build workflow without review.

---

## 24. IPC review checklist

Before adding a new command, answer:

1. What user-visible feature requires this command?
2. Is it narrower than a generic shell/filesystem capability?
3. Can the request target only an authorized repository?
4. Which arguments are user/repository controlled?
5. How are refs validated?
6. How are paths handled?
7. Could an argument be interpreted as an option?
8. Is the operation destructive?
9. Does it expose secrets or arbitrary process output?
10. Which tests demonstrate the boundary?

If these questions cannot be answered, the command is not ready.

---

## 25. Security-sensitive change rule

Changes affecting any of the following require explicit review against this document:

- Tauri capabilities
- IPC commands
- Git process invocation
- filesystem access
- URL opening
- credentials
- authentication
- destructive operations
- update mechanisms
- native plugins
- command/environment handling

Codex or another coding agent must call out such changes in its final summary.

---

## 26. Known limitations policy

Do not claim a repository mode is safe/supported until tested.

If Orbit does not yet support a case such as:

- bare repository
- linked worktree
- submodule workflow
- very large binary diff
- complex rebase state

the correct behavior is an explicit limitation or safe failure.

Do not guess through an unknown repository state.
