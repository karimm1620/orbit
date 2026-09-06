# Orbit M2 — Changes and Diff Research

**Status:** Accepted architecture for M2 implementation

**Scope:** Read-only working-tree changes and per-file diffs

**Evidence environment:** Linux 7.2.2 x86_64, native Git 2.55.0

This document records the Git experiments and architecture decisions that must guide M2. It does
not describe production M2 code; that implementation has not started.

---

## 1. Constraints inherited from M0 and M1

M2 keeps the existing trust boundary:

```text
React WebView
    -> purpose-specific typed IPC using opaque IDs
Rust repository/change services
    -> the existing hardened GitRunner
native system Git
    -> an already-authorized repository root
```

The WebView must not receive a generic Git runner, arbitrary filesystem access, raw path authority,
or the ability to construct revision/pathspec expressions. Repository paths, status output, patch
content, attributes, and configuration are untrusted.

M2 remains read-only. Staging, discard, checkout, conflict resolution, commit creation, and all
other mutations remain outside this architecture.

---

## 2. Controlled Git evidence

Disposable repositories were used rather than developer repositories. The installed Git manuals
for `git-status(1)`, `git-diff(1)`, and `gitattributes(5)` were also checked.

| Case | Observed behavior | Architectural consequence |
| --- | --- | --- |
| Staged and unstaged edits on one file | Porcelain v2 emitted one `1 MM ...` record. | Model the index and worktree facets independently on one change entry. |
| Staged rename to a name containing a tab | `-z` emitted a type `2` record with target and origin as separate NUL-terminated byte strings. | Parse raw bytes; never split paths on tabs/newlines or parse display quoting. |
| Spaces, Unicode, tabs, newlines, and a leading `-` | Porcelain v2 `-z` preserved every path byte without quoting. | Keep byte-exact paths in Rust and place path arguments after `--`. |
| Invalid UTF-8 filename byte `0xff` | Porcelain v2 preserved the byte. | IPC file identity cannot be a JavaScript string path. Use an opaque file ID and escaped display text. |
| Unstaged filesystem move | Git reported a tracked deletion plus an untracked addition, not a rename. | Orbit reports Git's state and does not guess unstaged renames from similar content. |
| Copy detection | With `status.renames=copies`, Git emitted a `C100` type `2` record when the source was an eligible candidate. | Parse copy records but retain bounded Git detection; do not implement a generic DAG/file matcher. |
| Unborn repository with staged file | `git diff --cached` produced a normal new-file patch against the zero OID. | No synthetic empty-tree revision is required from the frontend. |
| Binary staged and untracked files | `--numstat -z` emitted `-\t-` while the patch suffix used human prose. | Use numstat framing, not localized patch prose, to classify binary output. |
| Merge conflict | Status emitted `u UU ...`; ordinary `git diff` emitted a combined `diff --cc` patch. | Conflicts are an explicit change/diff state; the initial unified parser does not misparse combined diffs. |
| Untracked leading-`-` path | `git diff --no-index -- /dev/null ./-odd.txt` produced an all-add patch and exited `1`. | Accept exit `1` as the expected difference result only for the exact no-index operation. |
| Combined numstat and patch | `--numstat -z --patch` emitted a NUL-framed numstat prefix, an empty NUL record, then patch bytes. Rename/copy numstat used separate origin and target path fields. | One bounded Git process can classify and acquire a selected diff consistently. Validate the prefix against the Rust-owned entry before parsing the patch suffix. |
| Configured clean filter | An unprotected working-tree diff executed it. Empty `clean`/`process` overrides with `required=false` prevented execution. | Reuse M0 filter-driver discovery and neutralization for detailed status and working-tree diff reads. |
| Configured textconv and external diff | Unprotected diff commands created marker files; `--no-textconv --no-ext-diff` did not. | Both flags are mandatory for every M2 diff command. |
| Missing promised blob | An unprotected diff invoked a configured `ext::` promisor remote. `GIT_NO_LAZY_FETCH=1` plus `--no-lazy-fetch` failed without invoking it. | M2 requires the already capability-probed secure Git mode and fails closed. |
| Untracked symlink to `/etc/passwd` | No-index diff showed only the link target as mode `120000`; it did not dereference the target. | Symlinks may be represented, but Rust must retain root/path authority. |
| Untracked FIFO | A no-index diff blocked until the experiment's three-second timeout. | Revalidate file type immediately before acquisition and add a deadline to the existing GitRunner for M2 reads. Special files are unavailable, not diffable. |

The manual states that porcelain v2 tracked records are not ordered. Rust therefore sorts the
typed entries deterministically; the frontend must not infer meaning from Git's emission order.

---

## 3. Detailed status acquisition

### Command policy

M2 extends the existing status service rather than adding a second process boundary. The detailed
query is conceptually:

```text
git
  -c core.fsmonitor=false
  -c status.renames=copies
  -c diff.renameLimit=1000
  -c filter.<discovered>.clean=
  -c filter.<discovered>.process=
  -c filter.<discovered>.required=false
  --no-pager
  --no-lazy-fetch
  --no-optional-locks
  status
  --porcelain=v2
  --branch
  --untracked-files=all
  --ignore-submodules=dirty
  --find-renames=50%
  -z
```

Argument ordering may follow `GitRunner` conventions, but the effective options are required.
`--untracked-files=all` yields individual selectable files rather than collapsed directory
placeholders. `--ignore-submodules=dirty` deliberately avoids recursively scanning nested
submodule worktrees or their configuration; a changed gitlink commit may still be reported.

Copy/rename detection is explicit so repository configuration cannot silently change the status
model. The 1,000-candidate rename limit bounds the expensive fallback. When Git cannot detect a
copy/rename within that bound, Orbit presents the resulting add/delete states; it does not infer a
relationship itself.

### Parser

Rust parses the NUL stream as bytes and accepts only documented record forms:

```text
1 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path> NUL
2 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <score> <path> NUL <origPath> NUL
u <XY> <sub> <m1> <m2> <m3> <mW> <h1> <h2> <h3> <path> NUL
? <path> NUL
```

Branch headers are parsed as in M0. Ignored records are not requested. Unknown record kinds,
invalid fixed fields, invalid modes/OIDs/scores, duplicate incompatible records, incomplete
records, and exceeded limits are structured errors.

The M0 counts should be derived from this richer semantic parse so the summary and detailed file
list cannot drift. Each entry has independent optional staged and unstaged facets. Typed kinds are
`added`, `modified`, `deleted`, `renamed`, `copied`, `typeChanged`, and `untracked`; unmerged XY
combinations map to a separate closed conflict enum. Submodule flags and modes remain explicit.

The primary path is the porcelain record's current/target path. A rename/copy retains its origin
path. Records sharing the same primary byte path may be combined only when that produces one
unambiguous staged/worktree entry, such as a staged deletion followed by an untracked recreation.

---

## 4. Typed model and file identity

The intended model is semantic, not renderer-specific:

```text
RepositoryChanges
  repositoryId
  changeSetId
  head
  summary
  files: ChangedFile[]

ChangedFile
  fileId
  path: RepositoryPathDisplay
  originalPath?: RepositoryPathDisplay
  staged?: ChangeFacet
  unstaged?: ChangeFacet
  conflict?: ConflictKind
  submodule?: SubmoduleState

ChangeFacet
  kind
  oldMode?
  newMode?
  similarity?
```

Exact Rust/TypeScript names may be refined during implementation, but these invariants are locked:

- `changeSetId` and `fileId` are opaque, Rust-issued, process-local handles.
- Rust stores the byte-exact repository-relative current and origin paths behind those handles.
- React never submits a filesystem path, pathspec, diff option, revision, or object ID to request a
  working-tree diff.
- Display paths are typed text. Valid Unicode remains Unicode; control characters, backslashes,
  and invalid bytes receive a deterministic visible escape form such as `\\n`, `\\t`, and
  `\\xFF`. A flag distinguishes escaped-byte display from unchanged Unicode.
- File IDs are scoped to their repository and change set. They are opaque handles, not
  authorization secrets; repository authorization is checked on every request.
- Stable UI ordering is a Rust bytewise path order with a deterministic origin-path tie-breaker.

The purpose-specific IPC shape is conceptually:

```text
get_repository_changes(repository_id)
get_file_diff(repository_id, change_set_id, file_id, side)
```

`side` is a closed `staged | unstaged` enum. It is not a Git revision expression.

---

## 5. Diff acquisition

### Selected tracked change

Staged and unstaged data are intentionally separate:

- staged: `git diff --cached ... -- <Rust-owned paths>`
- unstaged: `git diff ... -- <Rust-owned paths>`

For rename/copy facets, Rust supplies the stored origin and target paths so Git can retain their
relationship. Other facets use the stored primary path. Detection options are explicit and
bounded. `--cached` is used directly for unborn repositories; the experiment confirmed that Git
compares the index against the empty side without a frontend-provided empty-tree OID.

Each selected tracked diff uses one bounded invocation with this effective output policy:

```text
--numstat -z --patch --full-index --no-color
--no-ext-diff --no-textconv
--unified=3 --diff-algorithm=myers
--find-renames=50% --find-copies=50% -l1000
--src-prefix=a/ --dst-prefix=b/
-- <Rust-owned path or origin/target paths>
```

The same `--no-pager`, `--no-lazy-fetch`, `--no-optional-locks`, fsmonitor override, and discovered
filter-driver neutralization used by the detailed status boundary are mandatory. The parser first
validates exactly one expected numstat entry (including any origin/target pair), then treats the
empty NUL record as the transition to the patch suffix. `-\t-` produces a binary state without
parsing human prose. Any unexpected extra file, path mismatch, or malformed transition fails as
malformed Git output or a stale change set.

### Untracked file

Untracked content has no index/HEAD side. Rust first resolves the stored file handle inside the
authorized root, uses non-following metadata to reject directories and special files, and then
runs the same bounded output format conceptually as:

```text
git diff --no-index <format and hardening options> -- /dev/null ./<Rust-owned path>
```

Exit `1` is the only additional success code and is accepted only for this exact operation. The
leading `./` and `--` make a leading `-` unambiguous. Linux `/dev/null` is the empty side for the
Linux-first M2 implementation; a future non-Linux port must provide a reviewed equivalent rather
than accepting a frontend path.

The symlink itself may be diffed, but it is not dereferenced. A final type check plus a 30-second
GitRunner deadline are both required because the type can change between status and acquisition.
Timeout, special-file, missing-file, and stale-file cases return bounded structured states/errors.

### Consistency

A single numstat-plus-patch process avoids classifying one filesystem version and parsing a patch
from another. A selected worktree file can still change concurrently; M2 promises a coherent
individual Git command result, not a filesystem snapshot. The refresh/change-set policy below
prevents an old response from being attached to a newer list.

---

## 6. Patch parser and response states

Acquisition and parsing both live in Rust. Git output is never sent to React as an unbounded raw
patch. A pure Rust parser converts the validated patch suffix to:

```text
FileDiff
  changeSetId
  fileId
  side
  content:
    TextPatch { additions, deletions, hunks, metadata }
    | Binary
    | TooLarge
    | Unavailable { reason }

DiffHunk
  oldStart, oldCount
  newStart, newCount
  heading?
  lines: DiffLine[]

DiffLine
  kind: context | addition | deletion
  oldLine?
  newLine?
  content
  noNewlineAtEnd
```

The parser validates unified hunk headers, line prefixes, old/new line accounting, bounds, and the
no-newline marker. It ignores patch-header paths for identity; only the Rust-held status entry is
authoritative. Mode-only and rename-only changes may validly contain metadata with zero hunks.

Patch text must be valid UTF-8 for the initial M2 viewer. Invalid patch content returns
`unsupportedEncoding`; it is never lossily decoded. This is an explicit state, not a claim that the
file is binary. Syntax highlighting is deferred.

Expected non-text states are:

- `Binary`: numstat reported `-\t-`; no binary patch or file bytes are returned.
- `Conflict`: status is unmerged; combined conflict patches are not passed to the unified parser.
- `Submodule`: the entry is a gitlink/submodule; nested repository content is not read.
- `TooLarge`: bounded stdout or parser collection limits were exceeded.
- `Unavailable`: special file, vanished path, unsupported encoding, changed/stale entry, timeout,
  or another specifically typed reason prevents a safe diff.

Structured command/capability/repository errors remain appropriate for failures affecting the
whole request. Raw stderr remains bounded diagnostic detail, not the UI contract.

---

## 7. Bounds

Initial M2 implementation limits are conservative acceptance limits, not performance claims:

| Resource | Initial limit |
| --- | ---: |
| detailed status stdout | 16 MiB |
| change entries | 10,000 |
| one raw relative path | 16 KiB |
| retained raw paths in one change set | 8 MiB |
| active change sets | 8 |
| change-set idle lifetime | 15 minutes |
| selected diff stdout | 4 MiB |
| parsed hunks | 10,000 |
| parsed diff lines | 100,000 |
| M2 Git read deadline | 30 seconds |
| stderr diagnostic | existing 64 KiB GitRunner limit |

Only the newest successfully produced change set for a repository remains current. Global change
sets use the same least-recently-used/idle-expiry style as history sessions; in-flight safe reads
need not hold a global registry mutex. If actual repository evidence makes a limit inappropriate,
change it deliberately with tests and documentation rather than silently removing the bound.

---

## 8. Refresh and stale state

`get_repository_changes` creates a point-in-time list and installs a new change set only after the
status parse succeeds. A failed refresh leaves the prior displayed list intact but marks the error
contextually. A successful refresh invalidates the prior repository change set and selected diff.

Before a diff read, Rust:

1. reauthorizes the repository ID and canonical root;
2. validates the change-set/file handles and requested side;
3. re-runs the hardened status read and confirms the stored entry/facet is still compatible;
4. revalidates the stored path and file type where a worktree read is involved;
5. runs the bounded diff command.

If the semantic entry no longer matches, the operation returns `change_set_stale` and the UI offers
refresh. Content may change while retaining the same status classification; the diff represents
the current content observed by its Git invocation. This is an explicit read-committed model, not
snapshot isolation.

React retains the M0/M1 request-generation guard. Repository switches and successful refreshes
discard responses carrying older change-set IDs. Selecting another file invalidates only the
selected-diff request. No polling or file watcher is approved in this phase.

---

## 9. Security review

| Threat surface | Required mitigation |
| --- | --- |
| Shell/argument injection | Direct `Command::new("git")` only; separate `OsString` arguments; `--` before every Rust-owned path. |
| Repository/path authority | Opaque repository, change-set, and file IDs; canonical root revalidation; no frontend path input. |
| Inherited Git control variables | Existing case-insensitive `GIT_*` scrub, followed only by trusted `GIT_NO_LAZY_FETCH=1`. |
| Partial-clone network/credentials/helpers | Capability-require global `--no-lazy-fetch`; fail closed before M2 status/diff reads. |
| Pager | Global `--no-pager`. |
| Optional locks | Global `--no-optional-locks`. |
| Content filters | Discover bounded configured driver names; override `clean`, `process`, and `required` exactly as M0 status does. |
| Filesystem monitor | `-c core.fsmonitor=false`. |
| Text conversion | `--no-textconv`. |
| External diff programs | `--no-ext-diff`. |
| Hooks | The selected status/diff commands are read-only and do not invoke hooks; tests must retain marker regressions. |
| Attributes/config changing format | Explicit numstat, patch, color, prefixes, context, algorithm, rename/copy threshold, and limits. |
| Special files / command stalls | Non-following type validation plus a bounded GitRunner deadline and child termination. |
| Output/memory pressure | Bounded stdout/stderr, entry/path/hunk/line limits, typed too-large state. |
| Untrusted rendering | Typed text only; no HTML/ANSI/URL interpretation and no `dangerouslySetInnerHTML`. |

No Tauri capability expansion is required. M2 adds only purpose-specific commands to the existing
Rust-managed application state.

---

## 10. Required implementation tests

Parser/unit coverage must include every porcelain record form; every XY/conflict combination;
staged-plus-unstaged entries; rename/copy origin framing; submodule flags; unusual and invalid-UTF-8
paths; duplicates; malformed records; and every bound.

Patch parser fixtures must cover linear hunks, omitted counts, multiple hunks, additions/deletions,
empty files, no-newline markers, mode-only and rename-only metadata, Unicode content, invalid UTF-8,
malformed headers/accounting, binary framing, path-header spoofing, and oversized output.

Real temporary repositories must cover staged/unstaged/both, unborn staged add, deletion,
rename/copy, untracked, conflict, binary, symlink, special-file rejection, and external edits between
list and diff. Security regressions must prove filters, textconv, external diff, fsmonitor, and a
promisor remote are not executed. A timeout fixture must not hang the suite.

Frontend pure-state tests should cover refresh invalidation, stale selected-diff responses,
switching files during an in-flight request, contextual errors, and preservation of the current
change list when a refresh fails. DOM/visual work comes after this native data path.

---

## 11. Dependencies and deferred work

No new production dependency is accepted. The status and patch grammars are small, Git-specific,
bounded, and independently testable in Rust. The standard library can implement the process
deadline with `Child::try_wait`, a short sleep, and `kill`; the implementation must continue
draining bounded stdout/stderr. Existing Vitest is sufficient for future pure frontend state tests.

Deferred beyond the initial M2 data path:

- syntax highlighting and a syntax-highlighting dependency;
- file watching or polling;
- full submodule worktree inspection;
- combined conflict diff rendering or a conflict editor;
- binary previews;
- non-UTF-8 text decoding selection;
- M3 staging, discard, and all mutation APIs.

The first implementation slice should add the byte-safe detailed status parser/service, opaque
change-set/file registry, typed IPC models, and real-repository/security tests. Diff acquisition and
the patch parser should follow only after that identity boundary is proven.
