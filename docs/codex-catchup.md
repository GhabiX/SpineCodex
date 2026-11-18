# Codex catchup

Move SpineCodex to an official Codex release while separating existing Spine
changes from version adaptations and preserving their history.

Use `version = vX.Y.Z` in branch names and `rust-vX.Y.Z` for the upstream tag.
The SpineCodex product version is separate: each completed catchup releases
`A.B.C -> A.(B+1).0` (for example, `0.4.2 -> 0.5.0`).

Before starting, inspect repository instructions, refs, remotes, and worktrees.
Use an isolated worktree and preserve other developers' uncommitted work.
Record full commit IDs, conflict decisions, and validation results in the task
worklog. Verify existing refs before resuming; do not silently overwrite them.

## 1. Freeze current main

Create the branch `history/pre-codex-{version}` at the committed current `main`
tip (`M`) **before** creating base or sync. Keep it immutable. A tag does not
replace this branch. Verify the previous official upstream base (`B`) from the
previous catchup history.

**Hard constraint: migrate only the Spine/fork semantics already present in
frozen main M. Do not add features on sync.** The official native changes in U
are the intended upstream upgrade; changes layered onto U must be the minimum
adaptations needed to preserve M behavior. Unmerged branches, experimental
implementations, candidate binaries, and newer design or formal definitions
are not sources of additional features. Record any difference between M and a
newer definition as a scope difference; implementing that feature is a separate
task. Passing compilation, tests, or rehearsals does not expand this scope.

## 2. Pin the official upstream base

Fetch the selected release tag from the official `openai/codex` repository.
Create `base/codex-{version}` at its peeled commit (`U`) and keep it immutable.
Use a pinned release, not a moving upstream branch.

## 3. Create sync from base

Create `sync/codex-{version}` at `U` in the isolated worktree. Leave `main` and
the frozen history unchanged until the user personally validates the candidate
and explicitly approves promotion as required in step 6.

## 4. Squash the existing Spine changes

Replay the complete `B..M` delta onto `U` with three-way conflict handling, then
commit it once as preparation commit `S`. Its only parent must be `U`.
Preserve upstream changes and Spine intent; do not overwrite changed upstream
files with old copies and restore them in later commits. Account for moved,
removed, generated, and binary files as well as source code.

Include these fields in the commit body:

```text
Frozen-Spine-History-Branch: history/pre-codex-vX.Y.Z
Previous-Main: <full M>
Previous-Codex-Base-Commit: <full B>
New-Codex-Base-Commit: <full U>
Pending-Adaptations: <remaining compatibility work>
```

Resolve textual conflicts now; keep version adaptations separately reviewable
in step 5. Preparation may still reference old APIs and need not compile.

## 5. Commit and validate the version adaptations

Continue on sync with focused commits for upstream API changes and affected
Spine integration, tests, schemas, packaging, and documentation. Exclude
unrelated features, native refactors, experiments, and cleanup.

Review `B..M`, `U..S`, `S..T`, and `U..T`, where `T` is the candidate sync tip:
confirm the frozen Spine delta is complete, upstream behavior is preserved,
and every subsequent change has a migration reason. For each behavioral
adaptation, identify the behavior retained from M, the changed upstream API,
the minimum required adjustment, and evidence that behavior is preserved.
Check actual content as well as commit ancestry to exclude features copied or
cherry-picked from outside M. Keep a final changed-path record with explanations
for conflicts and intentional exclusions.

Run the repository's build/test gates and validate feature-off native behavior,
open/close/next/spawn, per-sampling context replacement, transition persistence,
and supported resume/fork/rollback/AoT/compact paths. Record the source SHA,
commands, configuration, results, and untested paths. For live comparisons,
verify actual binaries, launch flags, network environment, and daemon versus
embedded backend; rerun affected checks when the candidate changes.

## 6. Promote main and publish the next minor release

Prepare the product version `A.(B+1).0` and release notes in a separate commit
on sync. Follow the current [release workflow](../.github/workflows/spine-release.yml);
do not confuse the product version with the upstream Cargo workspace version.
Validate and rehearse release artifacts from the final tip `T`.

After review and automated validation, the user must personally test candidate
T and explicitly approve its promotion and release. CI, a successful rehearsal,
or earlier general authorization to execute the catchup does not replace this
gate. Until then, do not reset or advance main, push or move a release tag, or
trigger production publication.

After that approval, set `main` to the approved `sync/codex-{version}` tip T.
Preserve dirty worktrees before moving refs. Publish history/base/sync/main so
the previous history remains reachable; guard any required non-fast-forward
update against concurrent changes. Never use an unconditional force push.

Publish the product tag `vA.(B+1).0` at `T` through the release workflow. Verify
the tag source, GitHub release, npm versions, provenance, and a clean install.
Record release links and failures; promotion alone is not a completed release.
A task scoped to preparation or validation stops at its requested boundary.

The resulting history is:

```text
history/pre-codex-{version} = M
base/codex-{version} = U
U -> S (one frozen-Spine squash) -> adaptation/release commits -> T
sync/codex-{version} = main = product release tag's commit = T
```

## External contributions

External catchups use the same six steps. **Maintainers own steps 1–4 and 6;
contributors perform step 5 through a pull request.**

1. The contributor requests to work on a specific upstream Codex catchup and
   agrees on the scope with maintainers before starting.
2. Maintainers complete steps 1–4 and publish `sync/codex-{version}` with the
   preparation commit and remaining adaptation work.
3. The contributor branches from that sync branch, implements incremental
   adaptations, validates them, and opens a PR **targeting
   `sync/codex-{version}`** with test evidence and known gaps. The PR contains
   adaptation commits, not a recreation of the preparation squash.
4. Maintainers review and merge the PR, perform further integration and release
   checks, then complete step 6 to promote main and publish the new minor version.
