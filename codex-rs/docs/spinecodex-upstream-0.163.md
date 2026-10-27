# Upstream 0.163.0 migration

Baseline: upstream `rust-v0.163.0`, commit
`83086188ae0614bd882818ff1aa23f3194962012`, whose parent is the upstream
cutoff `30c1058df67d3d28000cddb6faa8e80d9a9143e9`. The frozen source is branch
`history/pre-codex-v0.163.0` at
`cb01f18b0113ada388b0ede69b36c1194513a240`; its actual previous upstream base is
`rust-v0.153.4` (`3d2ee51ca2d5db578f328aa75e20aa22c0197c9a`).

The source candidate is `spinecodex-dev` commit
`28c1283874142005bcf4ea640c4b6f0646fec973`. Its intermediate 0.162.1 port is
candidate provenance, not this catchup's previous baseline. The branch consists
of U, one preparation squash of the frozen Spine delta, and focused adaptation
commits. Native upstream implementation is retained from U throughout preparation.

The independently developed Spine kernel optimizations, generic native test
infrastructure repairs, private drill harness and unregistered paths removed by U
are excluded. Kernel implementation remains the frozen main version; Plan-mode
instructions are aligned with U's tool availability. Native float decoders remain
because Spine's arbitrary-precision persisted records must coexist with typed
native float fields. Native fixture changes supply Spine fields or select Spine
Off; they do not introduce new native product behavior.

This document records migration choices, not a release or acceptance verdict.
Some sections below describe candidate tests and explicitly note their original
unvalidated status. Current verification and the per-path conflict/exclusion
ledgers are recorded separately in the outer CacheTree task
`tasks/codex_upstream_prepare_20261019_1425/evidence/repair-20261027/`.

## History and scope review

The preparation commit is `f4d8c36cff561bc14bb63c4f680269bc31916eec`.
It records all 547 frozen B-to-M paths; new U lines are retained during textual
conflict resolution. Follow-up commits adapt those Spine consumers. The rejected
whole-file transplant and filename-whitelist attempts are not ancestors of sync.

The review compares both preparation-to-tip and U-to-tip. A large native-looking
migration hunk was not sufficient evidence of new native work: the earlier
preparation had regressed U and the later commit restored it. The rebuilt
preparation retains U's feature catalogue and agent module split from the start.

| Path / surface | Required Spine delta against U |
| --- | --- |
| `features/src/lib.rs` | Existing Spine feature variants/entries and dependency rules; debug-only feature-off verification. All native registry variants remain U. |
| `analytics/src/reducer.rs` | Two exhaustive match arms ignore the new durable `ThreadItem::SpineTree` item. |
| `external-agent-migration/src/sessions/append.rs` | Ignore Spine rollout records as structural metadata and pass optional Spine history. Native history revision/retained-context APIs already come from U. |
| `core/src/agent/control.rs` and child modules | Spine sampling-boundary forks, batch reservations, limits and notification suppression use U's existing LocalAgentRuntime. |
| `core/src/tools/parallel.rs` | Direct Spine transition dispatch composes with U call state, execution metadata, cancellation and lifecycle interfaces. |
| `protocol/src/json_f32.rs`, `json_f64.rs` | Keep typed native floats readable when Spine enables arbitrary-precision persisted JSON; do not normalize opaque payloads. |
| `tui/src/app/agent_status_feed.rs` | Ignore Spine tree items in the native activity feed. |
| `tui/src/app/agents_overview.rs`, `reconnect.rs` | Use compatibility identity for the service connection while retaining independent Spine product display identity. |
| TUI realtime/voice fixtures | Explicitly select native mode or unwrap the current replay-event envelope; no native voice behavior is introduced. |
| CLI doctor | Product command/version references for the existing Spine distribution. |
| CLI queue fixture | Compose the native plugin flag with the shared feature table instead of duplicating Spine-off defaults. |
| CLI worktree fixture | Preserve exact user-prompt matching while accepting the existing Spine user anchor. |
| Native snapshots | Reviewed product identity/layout or explicit Spine-off fixture selection. Three unreferenced old snapshots are retired. |
| Generated protocol/SDK | Spine feedback/tree/progress types and durable history views. Live native method names are exactly U's; the unproduced Rust `thread/rolledBack` notification restored by the candidate is removed in favor of U's `thread/reverted`. Existing native Python SDK types stay as in U, with only Spine additions; this catchup does not clean up stale upstream SDK classes. |
| Spine kernel | Frozen main implementation; independent candidate optimizations are excluded. Only Plan-mode instructions follow U tool availability. |
| Release workflow | Frozen main workflow with the product-version source adapted to the workspace compatibility identity. Independent registry-wait optimization is excluded. |

Generic OAuth/login locks, exec-server diagnostics, Git-attribution test timing,
RMCP fixture warmups, daemon teardown/supervisor scripts and the private drill
harness are excluded. Retired tracked files are preserved in the outer recycle
bin; original code history remains on the frozen history branch. Test fixtures
must check errors from the newly fallible history-installation and managed-feature
APIs; a fixture must not silently proceed in the wrong mode or with absent history.

## Identities and removed interfaces

The Cargo workspace version is the compatibility version. Install-context derives
its compatibility constant and upstream tag from `CARGO_PKG_VERSION`; the upstream
commit is recorded separately. Product identity remains independent. See
[version identities](spinecodex-versioning.md).

Upstream removes shell-escalation and the pinned app-server zsh fixture. Do not
restore that package, wrapper or fixture to satisfy old validation setup. The npm
rehearsal audits the remaining native executable layout without requiring the
retired zsh payload; each required executable is still checked. Historical
rollout events remain readable; removed live rollback/fork APIs are not restored.

## Runtime integration boundaries

- Compaction installation now distinguishes a complete history from a prefix that
  may be rejected when its history generation, auth owner or suffix no longer
  matches. Spine's durable canonical history and model projection must remain
  consistent with the installation result. Persistence errors must not be treated
  as successful installation or harmless prefix rejection. Background compaction
  introduces another input path that must use the Spine projection when enabled.
  Background input exposes its captured prefix within the crate so session launch
  can supply native generation/cutoff coordinates without losing the captured
  projection. Local compaction reserves the next window and explicitly renders
  TokenBudget context for those IDs; it does not advance live state before
  checkpoint installation. This applies with Spine both off and on. Remote V2
  retains upstream's separate render-before-reserve ordering.
  The local-compaction tests compare the full typed TokenBudget fragment against
  checkpoint/live window IDs in both modes, and check that failed Spine checkpoint
  persistence leaves the reserved window uninstalled. These are test contracts,
  not a claim that the tests have run.
- Child startup is centralized upstream. Spine's checkpoint, projection context,
  project inheritance and failed-spawn capacity rollback must compose with current
  upstream fork options, rather than recreate the removed startup API. Spine spawn
  explicitly captures the spawning turn's disabled-plugin choices, as the native
  spawn handler does, including an explicit empty choice. Both single and batch
  preparation capture the resolved parent thread ID; execution consumes that ID
  instead of looking only at optional spawn options. Session-source-only forks
  therefore retain native parent MCP extension inheritance in OFF as well as ON.
- New optional response metadata must survive history reconstruction and
  projection. A missing field and a populated field need separate behavioral
  coverage; constructor fixes or successful compilation alone are insufficient.
  Spine context size-boundary fixtures explicitly omit message status and encrypted
  content to preserve their original wire shape. The compaction/Spine-record
  round-trip fixture instead populates both fields with synthetic metadata, so its
  full checkpoint equality also checks preservation in replacement and guardian
  history. These test updates have not yet been typechecked or executed.
  The debug redactor retains the new `elicitation_abandoned` event kind without
  exposing its server name or response token. Raw known-kind preflight and typed
  event names are generated from one exhaustive event table, including auth
  recovery and thread queue events. Historical task/turn tag aliases remain
  accepted; truly unknown kinds remain redacted and malformed known kinds remain
  distinguishable. This diagnostic API is mode-independent.
- App-server keeps upstream read-state receipt retirement during revert and Spine's
  deferred listener publication. A successful Spine revert publishes its rebuilt
  tree after `thread/reverted`; failed reverts restore listening without emitting a
  success notification. Fork responses use the current config snapshot's model
  and service tier while retaining Spine feedback metadata.
  Connection fixtures pass both capabilities from the initialized session into
  Spine's outbound-ready hook: the V2 lifecycle client opts into experimental APIs,
  while the workspace-routing client retains native non-experimental defaults.
  The routing fixture uses the native thread store (`thread_store_override: None`),
  leaving fault injection confined to revert tests. Upstream's pending-unload map
  holds a real watch sender until teardown removes the entry; the cold-resume
  listener fixture now models that same completion channel without removing its
  held-cleanup precondition or listener-registration assertions. These fixture
  migrations have not yet been typechecked or executed.
- Plan mode exposes no Spine transition tools, matching the existing execution
  boundary. Upstream permits non-mutating exploration but not plan-executing
  mutations; Spine's persistent branch transitions stay suspended in this mode.
  The bundled protocol explicitly exempts Plan from branch-creation requirements.
  Default mode retains its catalog and OFF adds no tools or protocol.
- The provisional new/resume header resolves product identity from the same
  bootstrap feature sources and managed requirements as final configuration,
  before the first frame. Reloaded local settings keep that identity hydrated;
  explicit/global OFF retains the native OpenAI header. No credentials or server
  startup are needed to choose the provisional brand.
- New canonical sampling commits append their turn-scoped tree view together with
  the canonical transition, and wait for the native writer's acknowledgement
  before installation/publication. Merely allowing an event in rollout policy
  is not sufficient: the observer channel bypasses that writer. The existing
  event format carries its owning turn; legacy history reduction and the native
  paginated item projection retain the latest `SpineTree` item for that turn.
  Resume routes this item through the existing TUI tree projection surface,
  gated by SpineJit, not a second file or private cache. The first authoritative
  snapshot is a display change even without a prior live snapshot: it creates
  pending history for the normal replay-buffer flush, not a bottom live tail.
  App-level cold-replay coverage drains the event queue and checks the actual
  history card's rendered lines; the OFF counterpart retains native history
  without restoring Spine UI. Transient spawn progress remains live-only. Old
  rollouts without durable tree views are not backfilled;
  standalone usage/compact observer updates are not historical cards. Their
  exact latest-state restoration is not covered by this forward-only fix.
- TUI reconnect errors leave through terminal cleanup. Reset retains upstream
  side-thread connections and pending approvals while clearing Spine projection
  and feedback state. Existing replay fences and independent capacity settings
  remain in place. The interrupted-turn spawn-progress regression is separate
  from the expanded reset test; merge hunks must not combine their bodies.
  The spawn-failure question gate routes answers with the request's thread ID and
  turn ID, through the same thread-scoped submission as native questions. Selection
  must not answer whichever thread happens to be active when the event is handled.

## Compaction test-contract migration

The prefix checkpoint adopts the model-visible suffix when Spine is enabled;
with Spine disabled it retains the native suffix. Reinstalling hidden native
content after a projected prefix would undo the projection. The prefix test
therefore captures its expected suffix from the same view as the sampled prefix,
while retaining stale-generation rejection and durable checkpoint assertions.

Upstream's compact wire format writes a full parallel metadata vector when any
replacement item has harness metadata. Missing entries then reload as
`Some(CodexHarnessMetadata::default())`. Tests derive that exact persisted
representation; they do not make `None` and `Some(default)` globally equivalent.
In particular, turn-input serialization rejects any metadata presence. Production
serialization and history installation are unchanged by this migration.

The metadata cold-replay fixture now supplies a real source before its successful
pre-compaction sampling. A successful empty-source generation violates the
planner's source-tail contract and prevented the later compact assertions from
running. The planner check remains unchanged. Product-code mutations of raw
suffix adoption, omitted TokenBudget context and lost encrypted compact-seed
content are used to check the migrated assertions.

## Cold-resume test-contract migration

R016 keeps the saved runtime workspace root across server restarts, including
when the selected named profile changes from workspace-write to read-only. The
profile identity is retained and its current definition is resolved; both legacy
and paginated histories remain covered. The old assertion that the workspace
selection disappeared contradicted upstream's owned-settings restoration.

The old `cold_resume_without_active_permission_profile_uses_current_config`
contract is replaced by upstream's five-case base-permissions test. Saved base
permissions, not a newly configured default, win when allowed. Saved approval
`Never` also wins over the new `OnRequest` default. Exact profile identity is
checked: full-access restoration and saved read-only have no active named profile;
deny-read rejects saved full access and selects `:workspace`; allowed-mode
constraints force read-only and clear the configured profile identity; a managed
catalog selects `:read-only`. No legacy API or permission fallback is restored.
Temporary product mutations omit saved workspace roots and base permissions to
check that the migrated tests observe the restoration behavior.

## Renderer fixture migration

Native status and selected session/startup fixtures explicitly disable SpineJit,
so their OpenAI title is independent of the process default. The separate status
brand test checks both title and color, including the global-OFF override of an
explicit fixture enable. Product version (0.4.0), release URL, npm update command
and the existing `spine-app-server-control` socket namespace remain independent
product identity. Each changed golden is reviewed and edited individually; narrow
update-URL wrapping and terminal padding are retained. No bulk snapshot acceptance
or production renderer change is part of this migration.

## Verification and evidence reuse

Validation is recorded with the final source tree, command, platform and result
in the task-local upgrade workspace. Previous candidate or private drill results
are not evidence that the repaired branch passed. The local gates cover protocol
exports, the frozen kernel, sampling/tool integration, compact/resume/fork and
TUI projection behavior. The companion can be reused only after verifying its
local dependency source and lockfile closure and its recorded artifact checksum.

Generic native fixture repairs, private release/drill procedures and independent
performance work are excluded. Historical product release notes are preserved;
this document records the current compatibility migration. Local validation does
not imply real-account or external-platform release verification.
