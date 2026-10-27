# codex-core

This crate implements the business logic for Codex. It is designed to be used by the various Codex UIs written in Rust.

## Spine local-host boundary

Native agent operations use `agent::api::AgentControl`. Spine batch admission,
status watching, continuation and subtree settlement use the local runtime's
`LocalAgentControl`; they do not extend the native controller trait. The Spine
execution limiter belongs to the shared runtime so fresh per-session handles
cannot admit an extra batch independently.

Spine sampling-boundary forks retain canonical source history through the latest
`SpineSamplingStarted` record. Paginated model-context projections are not a
substitute for that source. Native full-history forks still use the upstream
all/none contract; a fork during Spine sampling checkpoints the transformed
source using the upstream compaction resume and retained-context reconstruction.
This does not authorize copying parent-only retained grants into a child.

The `spine_user_input` integration suite compares the same mock questions with
Spine disabled and enabled. It checks the complete native tool schemas, question
and answer occurrences, and asynchronous replies as ordinary user input. The
existing upstream Plan-mode and asynchronous question suites remain separate.
Guardian reviewer configuration disables Spine and memory-projection features
before session creation, including when the parent requires them. Reviewer
instructions and tools must not inherit the parent's recursive execution policy.

Native compaction publishes replacement history before its best-effort rollout
append, retaining upstream behavior when a writer has already closed. Spine
compaction instead appends and flushes its checkpoint before publishing any new
history or context window. The `compaction_writer_failure` integration suite
closes an actual local thread writer and exercises these two contracts through
`Op::Compact`; the existing checkpoint unit test additionally checks the Spine
window and runtime epoch.

## Wine-exec integration tests

On x86-64 Linux, run the shared suite against the Windows exec server with
`bazel test //codex-rs/core:core-all-wine-exec-test`.

Local execution targets the host OS, Docker targets Linux, and Wine exec targets
Windows. Choose the skip macro by what the test depends on:

- `skip_if_target_windows!`: Windows target behavior.
- `skip_if_host_windows!`: Windows host constraints.
- `skip_if_remote!`: Local-only test behavior.
- `skip_if_no_remote_env!`: Remote-only test behavior.
- `skip_if_wine_exec!`: Wine-specific runner debt.

## Dependencies

Note that `codex-core` makes some assumptions about certain helper utilities being available in the environment. Currently, this support matrix is:

### macOS

Expects `/usr/bin/sandbox-exec` to be present.

When using the workspace-write sandbox policy, the Seatbelt profile allows
writes under the configured writable roots while keeping `.git` (directory or
pointer file), the resolved `gitdir:` target, and `.codex` read-only.

Network access and filesystem read/write roots are controlled by
`SandboxPolicy`. Seatbelt consumes the resolved policy and enforces it.

Seatbelt also keeps the legacy default preferences read access
(`user-preference-read`) needed for cfprefs-backed macOS behavior.

### Linux

Expects the binary containing `codex-core` to run the equivalent of `spine-codex sandbox` when `arg0` is `codex-linux-sandbox`. See the `codex-arg0` crate for details.

Legacy `SandboxPolicy` / `sandbox_mode` configs are still supported on Linux.
They can continue to use the legacy Landlock path when the split filesystem
policy is sandbox-equivalent to the legacy model after `cwd` resolution.
Split filesystem policies that need direct `FileSystemSandboxPolicy`
enforcement, such as read-only or denied carveouts under a broader writable
root, automatically route through bubblewrap. The legacy Landlock path is used
only when the split filesystem policy round-trips through the legacy
`SandboxPolicy` model without changing semantics. That includes overlapping
cases like `/repo = write`, `/repo/a = none`, `/repo/a/b = write`, where the
more specific writable child must reopen under a denied parent.

The Linux sandbox helper prefers the first `bwrap` found on `PATH` outside the
current working directory whenever it is available. If `bwrap` is present but
too old to support `--argv0`, the helper keeps using system bubblewrap and
switches to a no-`--argv0` compatibility path for the inner re-exec. If
`bwrap` is missing, it falls back to the bundled `codex-resources/bwrap`
binary shipped with Codex and Codex surfaces a startup warning through its
normal notification path instead of printing directly from the sandbox helper.
Codex also surfaces a startup warning when bubblewrap cannot create user
namespaces. WSL2 uses the normal Linux bubblewrap path. WSL1 is not supported
for bubblewrap sandboxing because it cannot create the required user
namespaces, so Codex rejects sandboxed shell commands that would enter the
bubblewrap path before invoking `bwrap`.

### Windows

Legacy `SandboxPolicy` / `sandbox_mode` configs are still supported on
Windows. Legacy `read-only` and `workspace-write` policies imply full
filesystem read access; exact readable roots are represented by split
filesystem policies instead.

The elevated Windows sandbox also supports:

- legacy `ReadOnly` and `WorkspaceWrite` behavior
- split filesystem policies that need exact readable roots, exact writable
  roots, or extra read-only carveouts under writable roots
- backend-managed system read roots required for basic execution, such as
  `C:\Windows`, `C:\Program Files`, `C:\Program Files (x86)`, and
  `C:\ProgramData`, when a split filesystem policy requests platform defaults

The unelevated restricted-token backend still supports the legacy full-read
Windows model for legacy `ReadOnly` and `WorkspaceWrite` behavior. It also
supports a narrow split-filesystem subset: full-read split policies whose
writable roots still match the legacy `WorkspaceWrite` root set, but add extra
read-only carveouts under those writable roots.

New `[permissions]` / split filesystem policies remain supported on Windows
only when they can be enforced directly by the selected Windows backend or
round-trip through the legacy `SandboxPolicy` model without changing semantics.
Policies that would require direct explicit unreadable carveouts (`none`) or
reopened writable descendants under read-only carveouts still fail closed
instead of running with weaker enforcement.

### All Platforms

Expects the binary containing `codex-core` to simulate the virtual
`apply_patch` CLI when `arg1` is `--codex-run-as-apply-patch`. See the
`codex-arg0` crate for details.

## Spine native replay regression coverage

The current upstream host no longer exposes the live rollback handler. Historical
`ThreadRolledBack` records remain part of native history replay. The five
`session::tests::native_replay_rollback_*` tests construct persisted markers
and restore them through `record_initial_history`; they cover dropped turns,
overlarge counts, previous-turn settings/reference context, compaction window
restoration, and cumulative markers. They do not recreate a live rollback API.

Three former tests are retired because their only subject was validation of
that removed handler: `thread_rollback_fails_without_persisted_thread_history`
(required live persistence), `thread_rollback_fails_when_turn_in_progress`
(active-turn rejection), and `thread_rollback_fails_when_num_turns_is_zero`
(live argument validation). No ledger behavior is removed with them. R036's
context-update boundary is covered by
`spine::tests::rollback_selected_prefix_trims_pre_turn_context_updates` and the
saved-history/follow-up request test
`native_replay_rollback_followup_trims_context_updates_and_spine_tree`. The
latter first sends a real developer setting and a nested Spine open in the
removed turn, appends a native durable marker after shutdown, then checks the
restored tree and the next model request. It resets the live collaboration
setting before that request: native history editing does not undo persistent
thread settings. The historical removed setting must be present before rollback
and absent after replay; this is not a prompt-wording assertion.

R059's `paginated_prompt_revert_publishes_retained_and_empty_spine_tree` drives the
native paginated store's same-thread revert and core reload, not the removed live
rollback handler. It verifies that reverting before the second prompt publishes
the retained canonical tree and reverting before the first prompt publishes the
empty root. Native replay must publish its current projection even when the
retained prefix has no canonical transitions; otherwise clients retain an old
tree. Full TUI notification ordering remains covered by the embedded app-server
test in the TUI crate.

R067's `upstream_162_world_state_realtime_risk_and_control_records_remain_redacted`
validates the fixtures against the native `RolloutItem` decoder before invoking
the public streaming debug redactor. World-state content, realtime transcript
identities, risk actions and context-compaction content must become their
content-erased shapes, not malformed-record placeholders.

The Guardian tool-plan fixture now uses the production reviewer configuration
and SDK resolution with the native internal Guardian source. Merely changing a
source tag on an already resolved parent SDK does not model reviewer startup.
The feature-off/Guardian wire test remains unchanged and checks the actual
outbound requests. Inter-agent communication is recorded through the same Spine
source append path as other history items, so projected and durable inputs stay
in step.
