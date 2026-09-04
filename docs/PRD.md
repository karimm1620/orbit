# Orbit — Product Requirements Document

**Version:** 0.1  
**Status:** Draft baseline  
**Product:** Orbit  
**Category:** Desktop Git client  
**Primary platform:** Linux  
**Core model:** local-first, no required account, no required backend

---

## 1. Product summary

Orbit is a fast, visual, local-first desktop Git client for developers who want a clearer view of Git without replacing Git's mental model.

Orbit should make everyday repository workflows easier to inspect and control:

- repository state
- commit history
- branches and tags
- working-tree changes
- staging
- diffs
- commits
- remotes
- merge and conflict state

Git remains the engine and source of truth.

Orbit is not intended to become a proprietary abstraction layer over Git. Concepts such as branches, commits, HEAD, the index/staging area, remotes, merge, rebase, and conflicts remain visible to users.

---

## 2. Product vision

Build a desktop Git client that feels like a modern developer tool:

- quick to launch
- lightweight enough to keep open all day
- visually clear
- keyboard-friendly
- useful without internet access
- useful without creating an Orbit account
- respectful of the user's existing Git configuration and credentials
- transparent about the Git operations it performs

Orbit should feel like a high-quality interface for Git rather than a replacement for Git.

---

## 3. Problem statement

Daily Git work is often distributed across several tools:

- terminal commands
- editor source-control panels
- web-based Git hosting
- history viewers
- diff viewers
- conflict tools

This fragmentation increases context switching.

Existing desktop Git clients can also introduce tradeoffs that Orbit wants to avoid:

- heavy resource usage
- cloud-first features that dominate local workflows
- mandatory or strongly encouraged accounts
- interfaces that hide important Git concepts
- noisy dashboards and excessive decoration
- weak Linux-first ergonomics
- poor behavior on lower-end hardware

Orbit's core proposition is simple:

> Make the local Git repository understandable and actionable from one fast desktop workspace.

---

## 4. Target users

### 4.1 Primary users

Developers who:

- use Git regularly
- understand basic Git concepts
- work with multiple branches
- benefit from visual commit history
- want a comfortable staging and diff workflow
- may use the terminal and Orbit side by side
- use Linux as a development environment

### 4.2 Secondary users

Orbit may also serve:

- developers learning Git
- students learning software development
- open-source contributors
- solo developers
- small development teams

Orbit must not oversimplify Git in a way that makes the tool frustrating for experienced users.

---

## 5. Platform strategy

### Phase 1

Linux is the primary development and validation target.

Arch Linux is a first-class development environment.

### Future consideration

After Linux is stable, Orbit may add:

- Windows
- macOS

Cross-platform ambitions must not force premature abstractions into the first Linux implementation.

---

## 6. Product principles

### 6.1 Local-first

Core repository workflows must work without:

- internet access
- an Orbit account
- an Orbit backend
- a third-party cloud API

### 6.2 No account required

A user should be able to install Orbit, open a local repository, and use the core product immediately.

Authentication for hosting providers is optional and feature-specific.

### 6.3 Git remains Git

Orbit keeps the real Git model visible.

Users should be able to understand:

- current branch
- HEAD
- commit hashes
- parents
- staging state
- local and remote refs
- ahead/behind state
- merge/conflict state

### 6.4 Visual-first, not decorative-first

Orbit prioritizes information hierarchy over dashboard decoration.

The UI should make relationships and repository state easier to read without filling the workspace with unnecessary cards, badges, gradients, or ornamental effects.

### 6.5 Keyboard-friendly

Common workflows should eventually be possible through keyboard navigation and shortcuts.

Mouse interaction remains fully supported.

### 6.6 Performance is UX

Orbit should behave well on lower-end hardware.

The product should avoid:

- loading full repository history unnecessarily
- rendering thousands of graph nodes without virtualization
- aggressive polling
- repeated Git commands for unchanged information
- long blocking operations on the UI thread

### 6.7 Safe by default

Potentially destructive operations require explicit and contextual UX.

Examples include:

- hard reset
- force push
- discarding changes
- deleting an unmerged branch
- cleaning untracked files
- aborting an in-progress operation

The UI must explain the effect rather than showing a generic confirmation.

### 6.8 Interoperable with existing tools

Orbit must coexist with:

- terminal Git
- IDE/editor Git integrations
- external file edits
- existing SSH keys
- Git credential helpers
- existing Git configuration

Orbit must refresh when repository state changes outside the application.

---

## 7. Core user journey

```text
Launch Orbit
      ↓
Choose or reopen repository
      ↓
Validate repository
      ↓
Open repository workspace
      ↓
Understand current state
      ↓
Inspect history / changes
      ↓
Stage / unstage
      ↓
Commit
      ↓
Branch / merge / sync as needed
```

A repository should become understandable within seconds of opening it.

---

## 8. Repository workspace

The exact visual layout is not locked in this PRD.

The workspace should provide clear access to:

- repository identity
- current branch / HEAD
- working-tree status
- history / graph
- changes
- branches
- remotes
- contextual details

A conceptual layout may look like:

```text
┌────────────────────────────────────────────────────────────┐
│ Orbit                           repository / current branch │
├──────────────┬──────────────────────────┬──────────────────┤
│ Navigation   │ Main workspace           │ Context details  │
│              │                          │                  │
│ Changes      │ graph / diff / workflow │ commit / file    │
│ History      │                          │ information      │
│ Branches     │                          │                  │
│ Remotes      │                          │                  │
└──────────────┴──────────────────────────┴──────────────────┘
```

The final design should be decided through implementation and design exploration, not forced by this diagram.

---

## 9. MVP definition

The MVP should prove that Orbit can serve as a practical local Git client for common daily workflows.

A user should be able to:

1. open a local Git repository
2. understand its branch, HEAD, remote, and working-tree state
3. inspect recent commit history visually
4. inspect file changes and diffs
5. stage and unstage changes
6. create a commit
7. inspect and switch branches
8. create a branch
9. fetch, pull, and push using existing Git authentication
10. understand operation failures through contextual error messages

---

## 10. Repository management

### 10.1 Open repository

The user can choose a local directory through a native system dialog.

Orbit must:

- verify that the selected location belongs to a Git work tree when appropriate
- resolve the repository/work-tree root
- reject unsupported or invalid selections with a useful message
- read initial repository metadata
- avoid exposing generic filesystem access to the WebView

### 10.2 Recent repositories

Orbit stores a small local list of recently opened repositories.

Minimum metadata:

- display name
- canonical path
- last-opened timestamp

The user can:

- reopen a recent repository
- remove an entry from Orbit's recent list

Removing an entry must never delete the repository from disk.

### 10.3 Repository state

Orbit should expose at least:

- repository root
- current branch when attached
- detached HEAD state
- HEAD object ID when available
- upstream when configured
- ahead/behind counts when available
- clean/dirty state
- staged file count
- unstaged file count
- untracked file count
- conflict count

---

## 11. Commit history and graph

Commit history is one of Orbit's defining features.

The graph should eventually represent:

- commit nodes
- parent relationships
- merge commits
- local branch labels
- remote branch labels
- tags
- HEAD

Commit rows should expose at least:

- subject
- short object ID
- author display name
- timestamp
- parent relationships

### 11.1 Interaction

The user can select a commit and inspect:

- full object ID
- subject and message
- author
- author/commit time where useful
- parent commits
- refs pointing to the commit
- files changed
- patch/diff metadata

### 11.2 Large repositories

Orbit must not require loading all history before the workspace becomes usable.

History loading should be incremental, paginated, or otherwise bounded.

---

## 12. Working tree

Orbit groups repository changes into meaningful states:

- staged
- unstaged
- untracked
- conflicted

It should correctly represent common statuses including:

- added
- modified
- deleted
- renamed
- copied
- untracked
- conflicted

Submodule behavior must be handled deliberately rather than silently treated as a normal file.

---

## 13. Diff viewer

Selecting a changed file should expose a useful diff.

Initial requirements:

- file identity
- addition/deletion hunks
- line additions
- line deletions
- old/new file metadata
- binary-file state
- unavailable/too-large state

Later improvements may include:

- inline versus side-by-side modes
- syntax highlighting
- word-level highlighting
- hunk staging
- line staging

---

## 14. Staging

Initial staging operations:

- stage file
- unstage file
- stage all
- unstage all

Later:

- stage hunk
- unstage hunk
- stage selected lines

A staging operation must be scoped to the currently authorized repository.

---

## 15. Commit workflow

Orbit should allow committing staged changes.

The commit workflow exposes:

- commit message input
- staged-change summary
- validation feedback
- operation progress
- success/failure result

Orbit should not impose Conventional Commits.

Optional assistance may later include:

- Conventional Commit suggestions
- recently used scopes
- repository-specific message patterns

Commit-message assistance must remain optional.

---

## 16. Branch workflow

Orbit displays:

- current branch
- local branches
- remote-tracking branches

Initial operations:

- create branch
- checkout/switch branch
- rename local branch
- delete local branch

Safety UX must consider:

- uncommitted changes
- unmerged commits
- detached HEAD
- an in-progress merge/rebase
- the currently checked-out branch

---

## 17. Merge and conflict workflow

Orbit should support basic branch merge operations.

Before a merge, the UI should clearly identify:

- source
- destination
- current repository state
- whether the operation appears fast-forwardable when known

When conflicts occur, Orbit switches into an explicit conflict state.

Initial conflict workflow may provide:

- conflicted-file detection
- open file externally
- mark resolved
- continue supported operation
- abort supported operation

A sophisticated built-in visual merge editor is not required for the first MVP.

---

## 18. Remote workflow

Orbit reads configured remotes and provides basic network operations:

- fetch
- pull
- push

Orbit should use the user's existing Git authentication where possible:

- SSH configuration and keys
- credential helpers
- Git configuration

Orbit must not build a custom credential store for ordinary Git remote authentication.

Network operations need:

- visible in-progress state
- cancellation when safely supported
- useful authentication/network errors
- refreshed ahead/behind information after completion

---

## 19. Git hosting integration

GitHub integration is not a dependency of the local Git experience.

A later optional integration may expose:

- repository metadata
- pull requests
- PR checks
- PR status
- issue references

Orbit's local workflows must remain useful if the user never enables a hosting-provider integration.

The architecture should not unnecessarily prevent additional providers later.

---

## 20. Command palette and shortcuts

A later productivity milestone should introduce a command palette, conceptually opened with:

```text
Ctrl+K
```

Candidate actions:

- open repository
- switch branch
- create branch
- fetch
- pull
- push
- stage all
- focus commit input
- open settings
- open terminal
- open editor

The exact shortcut map is not locked yet.

---

## 21. Search

Search may eventually cover:

- commits
- branches
- tags
- files

Advanced historical search is outside the first milestones.

---

## 22. Terminal and editor interoperability

Orbit does not try to replace the terminal or IDE.

Users should eventually be able to:

- open a terminal at the repository root
- open the repository or a selected file in a configured editor

The product must also tolerate repository mutations performed outside Orbit.

External changes should trigger or permit a safe refresh without requiring an app restart.

---

## 23. Refresh behavior

Repository changes can originate from:

- Orbit
- terminal Git
- an IDE
- file edits
- other local processes

Orbit should refresh efficiently.

The implementation must avoid aggressive polling by default.

The exact file-watching strategy is an architecture decision, not a PRD requirement.

---

## 24. Loading, empty, and error states

### 24.1 Loading

Operations that may take noticeable time need explicit progress state, including:

- reading history
- calculating large diffs
- fetch
- pull
- push
- clone, if clone is added later

The UI must remain responsive.

### 24.2 Empty states

Orbit needs purposeful empty states for:

- no repository opened
- repository with no commits
- clean working tree
- no remote
- no additional branches
- no search result

### 24.3 Errors

Errors should explain context and actionable cause when known.

Avoid:

```text
Operation failed.
```

Prefer:

```text
Could not switch to "feature/login".

Your working tree contains changes that would be overwritten.
```

Raw stack traces and unrestricted process output should not be the primary user-facing error surface.

---

## 25. Design direction

Orbit should feel modern without looking like a generic SaaS dashboard.

Avoid:

- excessive cards
- excessive glass effects
- decorative gradients with no information value
- excessive status chips
- unnecessary animation
- visual patterns that reduce code/diff readability

Prioritize:

- typography
- spacing
- hierarchy
- graph clarity
- diff clarity
- direct state indicators
- fast interaction

---

## 26. Visual identity

The name "Orbit" suggests relationships, paths, and connected objects.

The identity may subtly reference:

- orbits
- nodes
- trajectories
- connected systems

Branding should never reduce information density or legibility.

---

## 27. Theme and accessibility

Dark mode should be supported from the start.

Light mode may follow later.

Orbit must consider:

- keyboard navigation
- visible focus indicators
- scalable text/UI
- sufficient contrast
- readable code/diffs
- information that does not rely on color alone

Commit graph topology should remain understandable for users with reduced color discrimination.

---

## 28. Performance requirements

Performance is a functional product requirement.

Orbit should:

- bound history queries
- virtualize long lists/graphs when necessary
- avoid repeated identical Git commands
- avoid blocking WebView rendering
- avoid unnecessary background work
- cancel or supersede stale reads where practical

Exact numerical budgets should be introduced after the first working prototype can be profiled.

Performance claims must be evidence-based.

---

## 29. Security and privacy requirements

Orbit interacts with privileged local resources.

The product therefore requires strict boundaries.

At a product level:

- the WebView must not receive arbitrary shell execution
- the WebView must not receive arbitrary filesystem access
- Git operations must be exposed as purpose-specific capabilities
- repository paths and Git inputs are untrusted
- destructive operations require stronger confirmation
- repository content must not be uploaded to Orbit infrastructure
- the core product requires no telemetry
- credentials must not be stored in plaintext by Orbit

The binding technical rules are defined in `SECURITY.md`.

---

## 30. Offline behavior

Core local operations should work offline:

- open repository
- inspect history
- inspect changes
- diff
- stage/unstage
- commit
- branch operations
- local merge/conflict workflow

Network is required only for inherently remote functionality.

---

## 31. Non-goals for the initial product

### Built-in IDE

Orbit is not a code editor or IDE.

### Cloud repository storage

Orbit does not host user repositories.

### Required Orbit account

Core usage does not require an Orbit identity.

### Team collaboration platform

Orbit does not replace Git hosting services.

### Git server

Orbit does not host or serve repositories.

### AI-first workflow

Orbit is not an AI wrapper around Git.

AI may be explored as an optional helper in the future, but the product must remain complete and useful without an AI API.

### Reimplementing Git

Orbit should not implement its own version-control engine.

---

## 32. Product constraints

The initial product must:

- use local Git repositories as its source of truth
- prioritize Linux development
- work without a backend for core features
- work without an Orbit account
- remain useful offline
- respect existing Git configuration
- treat performance and security as product requirements
- preserve interoperability with terminal Git

---

## 33. Open questions

The following should remain open until they are needed:

- final UI composition
- exact graph-rendering technique
- state-management library
- file-watching implementation
- syntax-highlighting engine
- packaging formats
- update distribution strategy
- GitHub authentication flow
- Windows/macOS rollout
- optional AI functionality

These are resolved through `ARCHITECTURE.md`, `DECISIONS.md`, and milestone-specific research.

---

## 34. Product success

Orbit succeeds when a developer can leave it open throughout normal work and use it without feeling that it fights Git, hides important state, or consumes unreasonable resources.

The desired result is:

> Git stays the source of truth. Orbit makes that truth easier to see and control.
