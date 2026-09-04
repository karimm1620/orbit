# Orbit — Roadmap

**Strategy:** vertical slices, evidence-based progression, one reviewable milestone at a time.

Milestone numbering describes product sequence, not release version numbers.

---

# M0 — Repository Foundation

## Goal

Prove the complete trusted path from UI to native Git and back.

```text
user selects repository
        ↓
Rust/native dialog
        ↓
repository validation
        ↓
native Git
        ↓
typed snapshot
        ↓
React renders real data
```

## Scope

- Tauri + React + TypeScript project foundation
- pnpm workflow
- Rust Git process boundary
- native repository picker
- repository authorization/context
- Git executable detection/version
- repository root validation
- branch / detached HEAD state
- HEAD object ID
- upstream + ahead/behind where available
- working-tree counts
- recent bounded commit history
- loading / empty / error states
- basic recent-repository persistence if it remains small and safe
- initial automated tests
- initial CI

## Explicitly out of scope

- graph topology rendering
- staging mutations
- diff viewer
- branch mutation
- remotes mutation
- GitHub integration
- database
- large dependency set

## Acceptance

M0 is complete when a real local repository can be opened and Orbit displays real repository state with no mock data in the accepted path.

At minimum validate:

- normal repository
- clean work tree
- dirty work tree
- staged + unstaged + untracked files
- repository with no commit
- detached HEAD
- invalid/non-repository selection

Security review:

- no generic shell IPC
- no generic Git IPC
- no generic filesystem access
- repository-scoped model established

---

# M1 — Commit Graph

## Goal

Make history the defining visual experience.

## Scope

- bounded/incremental history loading
- graph topology/lane calculation
- branch/ref labels
- merge commits
- HEAD indicator
- virtualized or otherwise bounded rendering
- commit selection
- commit details
- keyboard selection basics
- performance profiling on a larger history

## Research gate

Before choosing a graph rendering dependency/technique:

- compare DOM/SVG/canvas/custom options
- estimate behavior with thousands of visible/loaded commits
- test on the primary development hardware
- record decision in `DECISIONS.md`

## Acceptance

- merges are visually understandable
- scrolling remains usable with a representative larger repository
- history does not require full-repo preloading
- selected commit details are correct

---

# M2 — Changes and Diff

## Goal

Make Orbit useful for inspecting daily edits.

## Scope

- staged list
- unstaged list
- untracked list
- conflicted indicator
- file status model
- file diff acquisition
- patch parser/model
- initial diff UI
- binary file state
- too-large/unavailable state
- refresh after external edits

## Acceptance

Correct behavior for:

- modified
- added
- deleted
- renamed
- untracked
- staged + unstaged version of same file
- unusual filenames
- binary file

---

# M3 — Staging and Commit

## Goal

Complete the edit → inspect → stage → commit loop.

## Scope

- stage file
- unstage file
- stage all
- unstage all
- commit message input
- commit execution
- hook failure handling
- post-commit refresh
- commit feedback
- keyboard flow

## Security gate

- file operations use pathspec separation
- commit messages never enter a shell string
- mutation commands are repository-scoped

## Acceptance

A user can perform a normal local commit workflow entirely through Orbit.

---

# M4 — Branch Workflow

## Goal

Support normal branch navigation and management.

## Scope

- local branch list
- remote-tracking branch list
- create branch
- switch branch
- rename local branch
- delete local branch
- safety state for uncommitted changes
- detached HEAD behavior
- branch-specific context in graph

## Acceptance

Branch actions reflect actual Git state and destructive deletion receives appropriate warning.

---

# M5 — Remote Workflow

## Goal

Support everyday synchronization without replacing Git authentication.

## Scope

- remote list
- upstream state
- fetch
- pull
- push
- ahead/behind refresh
- network/auth errors
- progress state
- cancellation where safely supported

## Security gate

- no credential capture/storage for native Git transport
- output/log redaction
- no shell invocation

## Acceptance

Fetch/pull/push work using the user's existing valid Git authentication setup.

---

# M6 — Merge and Conflict Workflow

## Goal

Make merge state understandable and recoverable.

## Scope

- merge branch
- conflict detection
- operation-in-progress state
- conflicted file list
- open externally
- mark resolved
- continue merge where applicable
- abort merge
- explicit destructive/recovery UX

## Deferred

A sophisticated three-way visual merge editor is not required here.

---

# M7 — Productivity Layer

## Goal

Reduce repetitive navigation.

## Scope candidates

- command palette
- shortcut system
- repository search
- commit/ref search
- open terminal
- open configured editor
- improved recent repositories
- repository refresh controls
- user preferences

Only add features that materially improve core Git workflow.

---

# M8 — Hosting Provider Integration

## Goal

Add optional hosting context without weakening local-first design.

## Initial provider

GitHub may be the first provider.

## Scope candidates

- provider authentication
- repository metadata
- pull request list
- PR status/checks
- open PR in browser

## Security gate

A dedicated provider-auth security review is mandatory before implementation.

Provider tokens require secure platform storage.

---

# M9 — Distribution and Polish

## Goal

Turn the development app into a reliable Linux product.

## Scope

- Linux packaging research
- app metadata
- icons/branding
- update strategy
- accessibility review
- crash/diagnostic behavior
- large-repository profiling
- startup/resource profiling
- package/binary size measurement
- release CI
- documentation

## Acceptance

Release claims are based on measured artifacts and tested environments.

---

# Future candidates

Not committed to a milestone yet:

- hunk staging
- line staging
- rebase workflow
- interactive rebase
- stash workflows
- tag management
- linked worktree UX
- submodule UX
- clone/init repository
- advanced history search
- side-by-side diff
- built-in merge editor
- multiple hosting providers
- optional AI assistance

These are not promises. They enter the roadmap only when product need is established.
