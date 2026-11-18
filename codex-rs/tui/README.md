# Spine integration boundaries

Local new, resume, and fork launches retain the pre-autostart policy: passively
probe an existing Spine socket only when the invocation can reuse its configuration;
otherwise select the embedded server using the current CLI and invocation config.
Profile selection and any CLI configuration overrides, including boolean feature
flags, use the embedded server. Explicit remote endpoints keep their existing
connection and error semantics.

Spine does not own a complete managed app-server installation/update channel.
The distribution capability therefore disables automatic managed startup even
when daemon_auto_start is enabled in configuration. Managed lifecycle entrypoints
reject before resolving shared upstream state. Ordinary local startup never
installs, restarts, updates, or takes over an official Codex daemon. This policy
restoration does not establish the cause or resolution of profile startup latency.

Native synchronous `request_user_input` uses the request-input overlay. Native
asynchronous questions use `bottom_pane/async_questions` and
`chatwidget/questions.rs`; Spine does not replace either editor or change their
ordinary user-turn answer delivery.

The upstream question editor, notification, and turn-end tests remain the native
UI coverage. Read-only history snapshots do not advertise feedback capability;
feature-menu seed items retain Spine controls alongside server discovery. Restore
status indicators through the shared state helper so header provenance survives.

Spine tree/activity notifications use the app-owned replay epoch and child-routing
state. Keep the `event_dispatch` and `spine_spawn_live` behavior tests alongside
visual snapshots: a snapshot alone cannot prove arrival ordering or delivery.
With SpineSpawn off, upstream `Interacted` activity does not change navigation
metadata or liveness. With it on, interactions refresh the canonical path and
running hint, but never revive a closed thread. The live activity regression
runs both profiles through the app notification consumer.
Feedback request generations also remain app-owned across widget replacement.
The `spine_p1` tests cover replacement followed by history replay and stale
feedback callbacks, and send screenshots through the real JSON-RPC dispatcher to
a local recording server without uploading a report.

Snapshot replay reserves its generation after the synchronous thread switch.
The queued transcript reset is a completion barrier: it clears previous queued
history without invalidating that new replay. A later switch or `/clear` still
invalidates stale generations, including when switching back to the same thread.
The `history_replay_fence_tests` drain the real app event queue for both cases;
cold-resume mock TUI checks separately verify question and answer UI retention.

The upstream PTY focus/provider/worktree integration fixtures explicitly disable
Spine features, so their native branding and behavior assertions exercise feature
off. Test Spine-enabled questions and tree transitions separately, not by weakening
native assertions to accept either product identity.

Prompt editing uses the native same-thread revert contract, not the retired
prompt-fork event. Its tests use paginated rollouts with native completed user
items (legacy preview-only rollouts cannot be reverted). They retain buffered-turn
selection, full composer/attachment state, and Spine tree truncation checks,
including the next outbound model request. The embedded tree test requires a
restored observer snapshot after the native revert notification; an absent
snapshot is a failure, not grounds for reconstructing a tree from rendered text.
Automatic tree history remains transparent to streamed answer and proposed-plan
consolidation; those tests preserve both content and surrounding cell identity.

TUI banners and update comparisons consume the independent product version.
Protocol compatibility remains separate: native service negotiation and version
notices use the compatibility version, while remote security-setup requests use
the compatibility user agent, not the product display version. Daemon update commands invoke
`spine-codex`; standalone and Homebrew automatic updates are not enabled for
this distribution, so installed-context notices link to its release options.

Compile this crate and run its selected tests in a fixed worktree/target slot.
A dependency compile failure is not a TUI test pass; do not accept snapshots to
hide event-ordering failures.

Frozen old test names are mapped to upstream's current behavior tests: MCP
in-progress replay now also checks completion; subagent approval routing includes
approval-first and foreign ownership; manual rename cancels a real delayed title
request. Reconnect uses delivery receipts instead of the old blanket manual-edit
hold. Warning replay keeps warning records while the approval stays foreground.
The retired world-writable startup scan is replaced by a delayed native sandbox
choice test that keeps OSC input away from protected decisions.

Feature persistence covers both the Spine switch and the native shell-tool
switch. Update/install fixtures check command formatting and product routing;
they do not establish downloaded package or installed-daemon behavior.
