# Orbit M3 — Staging and Commit Research

**Status:** Accepted architecture for M3 implementation
**Tested Git:** 2.55.0 on Linux
**Scope:** normal file/index staging, unstaging, and local commit creation only

**Implementation status:** M3-B1 staging/unstaging services implement the accepted mutation lease,
change-set fence, process-group runner, fixed commands, typed receipts, and post-status refresh.
Commit execution remains deferred to M3-B2.

This document records the evidence and decisions for Orbit's first repository mutations. It is
read with `SECURITY.md`, `DECISIONS.md`, and the existing M0–M2 architecture. The security model
wins if another document conflicts with this one.

M3 does not introduce a generic Git API. The accepted path remains:

```text
React intent + opaque handles
        ↓ purpose-specific typed IPC
Rust repository authorization + mutation lease
        ↓ fixed Git operation and Rust-owned byte paths
hardened GitRunner
        ↓
native Git + intentionally permitted mutation helpers/hooks
        ↓
fresh typed repository/change state
```

---

## 1. Research method

Experiments used disposable repositories under `/tmp`, direct Git arguments, marker executables,
and Git 2.55.0's installed command/manual documentation. They did not use developer repositories.
Fixtures covered born, unborn, and detached repositories; tracked, untracked, deleted, renamed,
copied, type-changed, and mixed-index files; byte-hostile names; configured helpers; standard
commit hooks; and deliberately stalled child processes.

Important observed behavior:

| Experiment | Git 2.55.0 result | Consequence |
| --- | --- | --- |
| `git --literal-pathspecs add --all -- '*.txt'` with another matching `.txt` file | Only the file literally named `*.txt` was staged. | All file mutation paths use global literal-pathspec mode and `--`. |
| Names beginning `:(glob)`, `-`, or containing spaces, tabs, newlines, Unicode, or byte `0xff` | Direct byte arguments staged and unstaged the exact path. | Reuse Rust-held M2 path bytes; never round-trip authority through JavaScript display text. |
| Modified, untracked, deleted, regular-to-symlink, and new symlink paths | `git add --all -- <path>` produced the expected M/A/D/T entries and staged symlinks without dereferencing them. | One fixed stage-file command covers the accepted ordinary facets. |
| A file with both staged and later unstaged edits | A second add replaced the index version with current worktree content. | “Stage file” consumes its current unstaged facet; it is not hunk staging. |
| `git restore --staged --source=HEAD` in an unborn repository | Failed with exit 128 because `HEAD` could not be resolved. | Unborn unstaging needs a separate fixed command. |
| `git rm --cached -f -- <path>` in an unborn repository | Removed the path from the index and kept the worktree file untracked. | Use this only for an authorized unborn staged addition. |
| `git read-tree --empty` in an unborn repository | Emptied the index and retained every worktree file. | This is the unborn-safe unstage-all operation. |
| `git restore --staged --source=HEAD` on born staged add/modify/delete | Restored the index to `HEAD` and retained worktree content, including the later unstaged part of a mixed file. | This is the born unstage-file operation. |
| Staged rename, then restore both target and origin | Became an unstaged deletion plus untracked target. | A staged rename must unstage both Rust-held endpoints. |
| Staged copy detected by status | Restoring only the copied target left the separately modified source staged. | A copy file action must not silently operate on its source entry. |
| `git add --all` from the worktree root | Included tracked modifications, deletions, untracked files, and a literal `*.txt` name. | Stage all takes no frontend pathspec and means the entire authorized worktree. |
| `git reset --mixed --no-refresh HEAD` | Restored the whole born index while leaving worktree changes. | This is the born unstage-all operation; the final hardened refresh replaces the skipped refresh. |
| `--no-optional-locks` with `git add` | The required index mutation still completed. | Keep the global no-optional-locks policy; required Git locks remain authoritative. |
| Changed submodule gitlink versus dirty-only submodule worktree | Adding the superproject path staged the changed gitlink; dirty-only nested edits remained unstaged inside the submodule. | Permit a gitlink commit change, but never present superproject staging as recursive nested staging. |
| Detached `HEAD` with staged content | A normal single-parent commit succeeded and remained detached. | Detached HEAD is supported and clearly reported, not treated as an error. |

### Configured process evidence

| Experiment | Result | Policy consequence |
| --- | --- | --- |
| Configured `filter.<driver>.clean` | `git add` executed the marker filter. | Clean/process filters are part of staging semantics and are intentionally preserved for an explicit stage action. |
| Configured `filter.<driver>.process` that exited during protocol startup | The marker ran and add failed. | Long-running process filters are also inside the mutation execution surface. |
| Configured `core.fsmonitor` | `git add` executed the marker. | Force `core.fsmonitor=false`; it is not needed to define staged content. |
| `post-index-change` | Add, restore, reset, and read-tree invoked it. Its nonzero exit did not undo the index update or make add fail. | Preserve this normal mutation hook and judge success from Git/current state, not hook exit assumptions. |
| Required clean filter failure under default add behavior | Add failed and did not install the otherwise stageable file. | Preserve filter failures and surface bounded diagnostics. |
| Unreadable file with `add.ignoreErrors=true` during stage all | Git exited 1 but partially staged a readable file. With `-c add.ignoreErrors=false`, it exited 128 with no partial index update. | Force `add.ignoreErrors=false` so repository config cannot opt Orbit into partial-success staging. |

M2 passive reads continue to neutralize all content-filter execution. M3 staging deliberately does
not inherit that read policy: filters can change the bytes written to the index and bypassing them
would make Orbit disagree with normal native Git. This execution is permitted only after a clear
user mutation intent, through a fixed operation, bounded runner, and authorized repository.

---

## 2. File identity and preflight

File mutation IPC reuses the M2 authority tuple:

```text
repository ID + change-set ID + file ID
```

`stage_file` implies the stored unstaged facet; `unstage_file` implies the staged facet. React does
not send a path, expected OID, pathspec, or side string. Before acquiring a mutation lease, Rust
validates the opaque handles against the authorized canonical root. Immediately before Git, it
runs the hardened detailed status read and requires the same byte path, relevant facet kind,
rename/copy endpoints, conflict state, and submodule policy to remain compatible.

Path selection rules are fixed:

- an ordinary path supplies only the stored current path;
- a rename facet supplies current and origin paths so both endpoints move together;
- a copy facet supplies only its current target, because changing the source would exceed the
  selected logical entry;
- conflict entries are rejected in M3 because staging them would become conflict resolution;
- a submodule gitlink commit change may be staged as the superproject path, but nested dirty-only
  state is not recursively staged;
- sparse-checkout exclusions retain Git's normal refusal; Orbit does not add `--sparse` silently.

All M3 mutation commands reject a repository with unresolved conflicts or an active
merge/rebase/cherry-pick/revert/am/sequencer/squash state. Stage-all in a conflict could otherwise
mark files resolved, and unstage-all could rewrite an operation's index; both belong to the later
conflict workflow. Staging an unrelated file during such an operation is also deferred rather than
creating a partial continuation API by accident.

This is read-committed mutation semantics, not a frozen worktree snapshot. If an external editor
changes bytes while the semantic facet remains compatible, Git stages the current bytes it opens.
Orbit does not copy file contents or claim TOCTOU-free staging. Structural changes cause a typed
refresh-required rejection where the fresh status can detect them.

Stage-all, unstage-all, and create-commit require the repository ID and current change-set ID even
though they accept no file ID. Rust compares a fresh semantic status with that change set before
acting. This avoids knowingly staging or committing a newly appeared structural change that the
displayed list did not authorize. Content-only races with the same status class remain subject to
native Git locking and the read-committed limitation above.

---

## 3. Accepted staging commands

All commands are assembled as separate OS arguments. The common prefix includes the existing
trusted environment scrub, `GIT_NO_LAZY_FETCH=1`, a required `--no-lazy-fetch` capability,
`--no-pager`, `--no-optional-locks`, and `-c core.fsmonitor=false`. Path commands place global
`--literal-pathspecs` before the subcommand and `--` before Rust-owned path bytes.

### Stage file

```text
git -c core.fsmonitor=false -c add.ignoreErrors=false
    --no-pager --no-lazy-fetch --no-optional-locks --literal-pathspecs
    add --all -- <rust-owned path> [<rust-owned rename origin>]
```

`--all` fixes modern add semantics for deletion as well as addition/modification. No `--force`,
`--sparse`, `--ignore-errors`, patch, or interactive mode is available.

### Stage all

```text
git -c core.fsmonitor=false -c add.ignoreErrors=false
    --no-pager --no-lazy-fetch --no-optional-locks
    add --all
```

The process current directory is the revalidated worktree root. With no pathspec, Git 2.55 defines
`--all` across the entire worktree. Ignored files remain ignored; Orbit does not force-add them.

### Configured behavior

Stage commands intentionally preserve attributes, line-ending rules, clean/process filters,
required-filter failures, safe-CRLF checks, index formats, and `post-index-change` hooks. They force
`add.ignoreErrors=false` and neutralize fsmonitor. Git aliases are not used because Orbit invokes a
built-in subcommand directly.

---

## 4. Accepted unstaging commands

### One path with an existing HEAD

```text
git -c core.fsmonitor=false
    -c submodule.recurse=false
    --no-pager --no-lazy-fetch --no-optional-locks --literal-pathspecs
    restore --staged --source=HEAD --no-recurse-submodules -- <rust-owned path(s)>
```

This changes only the index. A staged add becomes untracked, a staged deletion becomes an
unstaged deletion, and a mixed file retains its worktree edit.

### One path with unborn HEAD

```text
git -c core.fsmonitor=false
    -c submodule.recurse=false
    --no-pager --no-lazy-fetch --no-optional-locks --literal-pathspecs
    rm --cached --force --quiet -- <rust-owned path>
```

This branch is permitted only when fresh status says HEAD is unborn and the selected staged facet
is an addition. It does not delete the worktree file.

### All paths with an existing HEAD

```text
git -c core.fsmonitor=false
    -c submodule.recurse=false
    --no-pager --no-lazy-fetch --no-optional-locks
    reset --mixed --no-refresh --quiet --no-recurse-submodules HEAD
```

`HEAD` is a fixed Rust command token, not a frontend revision. Resetting the index to the current
HEAD does not move HEAD or modify worktree files. `--no-refresh` avoids redundant work because the
mutation service performs its own hardened post-operation status read.

### All paths with unborn HEAD

```text
git -c core.fsmonitor=false
    -c submodule.recurse=false
    --no-pager --no-lazy-fetch --no-optional-locks
    read-tree --empty
```

This empties the unborn index without enumerating paths. Git 2.55's installed config documentation
states that `submodule.recurse` applies to restore, reset, and read-tree, so every unstage command
forces it false (and uses `--no-recurse-submodules` where available). Unstaging the superproject
must never reset a nested submodule worktree. Every unstage operation preserves
`post-index-change`; Orbit does not provide a hook-bypass option.

---

## 5. Commit message transport and command

The accepted normal-commit command has no pathspec, revision, `-a`, amend, merge-continuation, or
hook-bypass option:

```text
git -c core.fsmonitor=false
    --no-pager --no-lazy-fetch --no-optional-locks
    commit --quiet --no-status --no-verbose --cleanup=verbatim --file=-
```

Rust writes one validated message to the child's piped stdin on a dedicated bounded writer and
closes the pipe. Stage/unstage operations keep stdin closed. The message never enters a shell or
command-line option.

Git 2.55 preserved a 108-byte fixture exactly at the commit-object level, including paragraphs,
quotes, backslashes, a leading `-`, option-like text, Unicode, and blank lines, when the input ended
with a newline. With no terminal newline, Git added its normal final newline. An embedded NUL was
rejected with exit 128. With `--cleanup=verbatim`, an all-blank message was accepted, so Orbit must
not delegate product validation to Git.

Rust therefore:

- accepts UTF-8 text up to 64 KiB;
- rejects NUL;
- rejects a message with no non-whitespace Unicode character;
- sends all other leading/trailing spaces and newlines unchanged;
- documents Git's terminal-newline normalization rather than trimming input.

`--file=-` prevents editor and template launch. A marker `core.editor` was not executed and
`commit.template` had no effect. The explicit cleanup and no-verbose options override
`commit.cleanup` and `commit.verbose` so config cannot append a diff to message preparation.

Git still writes its normal worktree-specific `COMMIT_EDITMSG` so prepare/commit hooks can inspect
or modify it, and the file can remain after a failed commit. Orbit creates no additional plaintext
temporary message file, does not persist a draft in M3, and does not log message content.

---

## 6. Hooks, signing, and configured execution

Controlled hooks ran in this order for a normal stdin-backed commit:

```text
pre-commit
prepare-commit-msg (source = message)
commit-msg
post-commit
```

`prepare-commit-msg` and `commit-msg` both modified the final committed message. A failing
pre-commit or commit-msg prevented commit creation and Git returned exit 1 with the hook's bounded
stderr (not the hook's original exit number). A post-commit exit 7 did not change Git's successful
exit or undo the commit, matching the installed hook documentation.

Git does not expose the ignored `post-commit` or `post-index-change` exit status to Orbit. A
successful Git exit plus verified state remains `applied`; nonempty bounded stderr can be retained
as a warning, but Orbit cannot invent a hook-failure category when Git supplies no signal.

Orbit intentionally does not pass `--no-verify`. Repository-local or configured `core.hooksPath`
hooks are allowed to execute only for an explicit mutation. Hook output is bounded, stripped of
control characters by the structured diagnostic layer, and never rendered as ANSI or HTML.

`commit.gpgSign=true` executed the configured `gpg.program` marker; its failure caused Git exit
128 before a commit. Passing `--no-gpg-sign` suppressed it, but Orbit will not do that silently.
Configured signing is normal user commit policy and remains enabled. An unavailable, rejected, or
interactive signer is handled by the mutation deadline and typed result. M3 does not add a signing
key picker or a “skip signing” control.

The security distinction is explicit:

1. passive status/history/diff reads continue to prevent filters, fsmonitor, textconv, external
   diff, hooks, pager, and lazy fetch execution;
2. a clear stage/unstage/commit action may execute only Git-native helpers intrinsic to that fixed
   mutation: staging filters, `post-index-change`, commit hooks, and configured signing.

Fsmonitor and pager remain neutralized because neither defines mutation content. Lazy fetching
remains fail-closed. A hook is arbitrary user/repository code and can itself start other programs
or network operations; Orbit does not claim to sandbox it. The UI must identify hook/signing
failures as consequences of the requested mutation rather than unexpected passive reads.

---

## 7. Normal commit eligibility

Before launching `git commit`, Rust revalidates the repository and current change set, then uses
fresh hardened status to require:

- at least one staged facet;
- no unresolved conflict;
- no active merge, rebase, cherry-pick, revert, or `git am`/sequencer state;
- a valid bounded message.

Git 2.55's porcelain v2 output did not identify a cleanly staged merge-in-progress. A plain commit
in that state created a two-parent merge commit. A plain commit after resolving a conflicted
cherry-pick also completed a commit. Porcelain alone is therefore insufficient.

Operation detection uses only fixed Rust probes. Pseudorefs such as `MERGE_HEAD`,
`CHERRY_PICK_HEAD`, `REVERT_HEAD`, and `REBASE_HEAD` are checked with bounded `rev-parse --verify
-q`. Worktree-specific state such as `rebase-merge`, `rebase-apply`, `sequencer`, and `SQUASH_MSG`
is located with `git rev-parse --path-format=absolute --git-path <fixed-name>` and checked without
following links. In a linked-worktree experiment Git resolved these under
`.git/worktrees/<worktree>/...`, so Orbit does not assume `.git` is a directory.

Detached HEAD and unborn HEAD are accepted when the other requirements pass. `git diff --cached
--quiet --exit-code` returned 0 for an empty born or unborn index and 1 for an unborn staged file;
production eligibility nevertheless reuses the already hardened semantic status rather than a
second diff parser.

M3 does not create empty commits, amend, continue/abort an operation, or resolve conflicts. A
rejected in-progress state directs the user to complete it with an appropriate tool until Orbit's
later conflict workflow exists.

---

## 8. Mutation deadlines and uncertain outcomes

The existing M2 deadline kills and reaps the direct Git child, which is sufficient for passive
commands that do not permit configured helpers. It is insufficient for M3:

- killing Git during a sleeping clean filter left the filter alive, left `index.lock`, and did not
  stage the file;
- killing Git while `post-index-change` slept left the hook alive, but the index was already
  updated;
- killing Git in `pre-commit` left the hook alive and did not create a commit;
- killing Git in `post-commit` left the hook alive after HEAD had already advanced.

A separate-session experiment placed Git and its normal descendants in one process group. Killing
the group terminated the helper. Sending TERM to the group first allowed Git to remove
`index.lock`; immediate KILL left that lock. M3 therefore extends the one `GitRunner` boundary with
an opt-in mutation execution mode:

1. create a new process group using the Linux standard-library process configuration;
2. supply either closed stdin or the bounded commit-message pipe;
3. drain bounded stdout/stderr concurrently;
4. at the deadline, send TERM to the process group and allow a two-second grace period;
5. send KILL to the group if Git remains, wait/reap Git, and finish draining pipes;
6. make mutation pipe readers cancellation-aware/nonblocking so a descendant that deliberately
   leaves the process group while retaining a pipe cannot hang Orbit's reader join;
7. never delete a possible Git lock file automatically.

Process-group signalling requires a small direct Unix syscall binding. A direct `libc` dependency
is accepted for the implementation if needed; it is already present transitively, adds no new Git
or UI abstraction, and is preferable to launching a second `kill` process or maintaining ad-hoc
unsafe constants. Non-Linux ports require an equivalent reviewed job/process-tree strategy.

Process groups stop Git and ordinary filter/hook/signer descendants; they are not a sandbox. A
marker hook that deliberately launched `setsid` remained alive after the original Git group was
terminated. Cancellation-aware pipe draining keeps Orbit's own deadline bounded, but M3 cannot
promise to contain arbitrary code that the user explicitly authorized through Git configuration.
Strong containment would require a separately researched Linux sandbox/cgroup policy and is not
implied here.

Initial limits are guardrails, not performance claims:

| Resource | Initial limit |
| --- | ---: |
| commit message | 64 KiB UTF-8 |
| mutation stdout | 1 MiB retained/drained |
| mutation stderr | existing 64 KiB runner bound |
| stage/unstage deadline | 120 seconds |
| commit/hook/signing deadline | 300 seconds |
| TERM grace before KILL | 2 seconds |
| concurrent mutations per repository | 1 |
| concurrent mutations process-wide | 4 |

After Git starts, nonzero exit, timeout, output overflow, or reader failure cannot universally mean
“nothing changed.” Hooks and filters may have modified the index, worktree, refs, or message; a
post-commit timeout may follow a completed commit. Orbit must not automatically retry.

The mutation service compares pre/post HEAD and performs a fresh status read when possible. It
returns a typed outcome:

- `applied`: Git exited successfully and the expected state transition was verified;
- `rejected`: a preflight or Git rejection is known, with bounded actionable detail and refreshed
  state where possible;
- `uncertain`: the process was terminated, output exceeded a bound, or post-state could not prove
  whether the requested mutation applied.

If HEAD advanced despite a non-success process result, commit creation is reported as applied with
a warning, not as safe to retry. A remaining lock is a contextual diagnostic; Orbit never removes
it because ownership cannot be proven.

---

## 9. Mutation serialization and races

Add a bounded process-local mutation registry owned by `RepositoryRegistry`. It grants at most one
lease for an authorized repository and four leases process-wide. The lease is acquired before the
final preflight and held across Git plus post-state refresh. A second stage, unstage, or commit for
the same repository receives a recoverable `mutation_in_progress` result. The registry mutex is
never held during Git I/O; an RAII token releases the slot on every return path.

This ordering prevents duplicate button activation, commit racing with stage, and worker
completion order from reordering Orbit mutations. Git's own required index/ref locks remain the
authority against terminal/IDE Git. Orbit cannot serialize external editors or processes and does
not claim transaction isolation across preflight and the Git subprocess.

Immediately before Git starts, the changes registry advances a Rust-owned invalidation generation
and retires the authorizing change set. This prevents an older in-flight detailed-status request
from reinstalling pre-mutation handles. File diff responses are also rejected by frontend request
generation once mutation begins. A later explicit refresh is allowed to supersede older reads.

History sessions are invalidated whenever post-state shows HEAD changed, including a hook-created
commit or uncertain commit outcome. Stage/unstage normally retain the captured history snapshot,
but the frontend refreshes repository metadata after the mutation.

---

## 10. Refresh and typed IPC contract

Purpose-specific commands are accepted conceptually as:

```text
stage_file(repository_id, change_set_id, file_id)
unstage_file(repository_id, change_set_id, file_id)
stage_all(repository_id, change_set_id)
unstage_all(repository_id, change_set_id)
create_commit(repository_id, change_set_id, message)
```

Exact Rust/TypeScript names may follow established conventions, but their authority must not grow.
There is no path, revision, Git argument, hook flag, signing flag, or process option in IPC.

Once Git was launched, the command returns a typed mutation receipt rather than losing post-state
inside an exception:

```text
MutationReceipt {
  operation,
  outcome: applied | rejected | uncertain,
  commitOid?,
  issue?,                 // stable code + bounded sanitized detail
  repositoryChanges?,    // newly installed opaque change set
  refreshRequired,
}
```

Authorization, malformed IDs/message, busy lease, stale preflight, and spawn failure can remain
structured IPC errors because no Git mutation began. A started command invalidates prior change
handles on every outcome. The service attempts one hardened detailed-status refresh and installs a
new change set with the existing generation ordering. If refresh fails, the receipt keeps the
mutation outcome but sets `refreshRequired`; the UI must not continue treating old handles as
valid.

On applied stage/unstage, React atomically replaces the Changes list and summary from the returned
change set. On commit or detected HEAD movement, React starts a fresh repository/history session
and clears selection/diff state. It does not optimistically claim success before the receipt.
Failures remain contextual, but last-known display data is labelled stale/non-authoritative once
its handles have been retired.

Frontend request generations still prevent a stale mutation response from overwriting a later
repository switch. Backend leases and invalidation remain authoritative; they do not rely on
React completion order.

---

## 11. Error model

M3 adds stable, operation-specific categories without making UI behavior depend on localized Git
stderr:

- `mutation_in_progress`
- `mutation_stale` / refresh required
- `mutation_not_applicable` (missing side, conflict, unsupported submodule/sparse state)
- `commit_message_invalid`
- `commit_has_no_staged_changes`
- `commit_operation_in_progress`
- `mutation_rejected` (filter, hook, signing, lock, permissions, identity, or other Git failure)
- `mutation_timed_out`
- `mutation_outcome_uncertain`
- `post_mutation_refresh_failed`

Bounded sanitized stderr may be diagnostic detail. Hook/configured-process output is untrusted:
control characters are removed and it is rendered only as text. The receipt tells the UI whether
retry is safe; uncertain operations are never automatically retried.

---

## 12. Rejected alternatives

- **Frontend paths/pathspecs:** violates opaque M2 authority and mishandles byte paths.
- **Hand-escaped pathspec syntax:** unnecessary and less reliable than Git's global literal mode.
- **`git reset HEAD -- <path>` for every unstage:** has the same unborn HEAD failure and a less
  explicit worktree/index contract than restore.
- **`git restore --staged` for unborn repositories:** empirically fails.
- **One synthetic empty-tree OID from React:** leaks revision construction and is hash-format
  sensitive.
- **`git commit -m <message>`:** message becomes a command-line argument and can hit platform
  argument limits; stdin is a narrower data transport.
- **Orbit temporary message file:** adds plaintext lifecycle and cleanup without improving hooks.
- **`git commit-tree`:** bypasses the normal commit-hook/signing/ref-update workflow.
- **Silently disabling filters/hooks/signing:** changes normal Git mutation semantics and hides
  user policy.
- **Allowing fsmonitor during mutation:** it adds execution without defining index content.
- **Killing only Git:** leaves permitted helpers running.
- **Treating all nonzero exits/timeouts as rollback:** contradicted by post-index/post-commit
  experiments.
- **Deleting `index.lock` after timeout:** Orbit cannot prove lock ownership or repository state.
- **Optimistic frontend mutation:** can present success before filters/hooks/Git have accepted it.
- **Database-backed mutation queue:** unnecessary for bounded process-local serialized work.

---

## 13. Implementation slices

### M3-B1 — Staging/unstaging mutation service

- add mutation leases and mutation-aware change-set invalidation;
- add process-group mutation execution with null stdin and bounded outcomes;
- add the shared fixed operation-state guard for every M3 mutation;
- implement fixed stage/unstage file/all commands and post-state refresh;
- add byte-path, stale-handle, config-helper, hook, timeout, and concurrency regressions;
- add purpose-specific typed IPC/TypeScript wrappers without UI integration.

### M3-B2 — Commit service

- add bounded stdin transport and message validation;
- reuse the operation-state guard and add staged-content commit eligibility;
- preserve and test commit hooks and configured signing;
- verify HEAD/outcome, invalidate history, and return refreshed typed state;
- add hook rejection/modification, detached/unborn, timeout, output, lock, and race regressions.

### M3-C — Staging and commit UI

- add stage/unstage controls, stage-all/unstage-all, commit input, busy feedback, and keyboard flow;
- consume mutation receipts without optimistic authority;
- make rejected, uncertain, stale, and refresh-required states explicit and accessible.

### M3-D — Final security, regression, and polish gate

- re-audit every command, environment/config surface, lease transition, and invalidation path;
- run real-repository desktop smoke where the native environment permits it;
- profile only when a recorded measurement task is performed; do not infer performance.

No production mutation is implemented by this research phase.
