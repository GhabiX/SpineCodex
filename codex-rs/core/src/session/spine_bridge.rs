use super::*;
use crate::context_manager::ContextAppend;
use crate::session::rollout_reconstruction::ReplacementHistoryBoundary;
use crate::session::spine_tree_inside::annotate_spine_tree_snapshot;
use crate::session::spine_tree_inside::build_spine_tree_context_annotations;
use crate::session::spine_tree_inside::build_spine_tree_inside_view_from_projection;
#[cfg(test)]
use crate::spine::IntoSpineNodeMemory;
use crate::spine::LiveRootCompact;
use crate::spine::SpineCloneBoundary;
use crate::spine::SpineCompactEvidence;
use crate::spine::SpineCompletedToolCallHostOutcome;
use crate::spine::SpineCompletedToolCallOutputEvidence;
use crate::spine::SpineHostEffects;
use crate::spine::SpineInitEvidence;
use crate::spine::SpineMessageEvidence;
use crate::spine::SpineObservedContextItem;
use crate::spine::SpineRootCompactHistoryPublication;
#[cfg(test)]
use crate::spine::SpineRootCompactHostInstall;
use crate::spine::SpineRootCompactHostOutcome;
#[cfg(test)]
use crate::spine::SpineRootCompactResult;
use crate::spine::SpineRuntime;
use crate::spine::SpineStore;
use crate::spine::SpineToolCallEvidence;
#[cfg(test)]
use crate::spine::SpineToolOutputRecording;
use crate::spine::SpineToolcallCommitEvidence;
use crate::spine::SpineToolcallCommitHostLoop;
use crate::spine::SpineToolcallCommitProviderInputTokens;
use crate::spine::SpineTrimOutcome;
use crate::spine::hooks;
use crate::spine::is_non_toolcall_msg;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::TokenUsageInfo;
use codex_protocol::spine_tree::SpinePlannedNodeSnapshot;
use codex_protocol::spine_tree::SpineTreeNodeSnapshot;
use codex_protocol::spine_tree::SpineTreeUpdateEvent;
use codex_rollout::should_persist_response_item;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

pub(super) struct PreparedSpineReplay {
    raw_len: u64,
    runtime: Option<SpineRuntime>,
    materialized: Option<Vec<ResponseItem>>,
}

impl PreparedSpineReplay {
    pub(super) fn new(
        raw_len: u64,
        runtime: Option<SpineRuntime>,
        materialized: Option<Vec<ResponseItem>>,
    ) -> Self {
        Self {
            raw_len,
            runtime,
            materialized,
        }
    }
}

#[derive(Debug)]
#[cfg(test)]
pub(crate) struct SpineToolCommit {
    recording: SpineToolOutputRecording,
    deferred_tree_update: Option<SpineTreeUpdateEvent>,
}

pub(crate) struct SpineRootCompactPublish {
    publication: SpineRootCompactHistoryPublication,
}

pub(crate) enum SpineToolcallTurnError {
    Codex(CodexErr),
    Terminal(String),
}

#[cfg(test)]
impl SpineToolCommit {
    pub(crate) fn recording(&self) -> SpineToolOutputRecording {
        self.recording
    }

    pub(crate) fn skips_host_recording(&self) -> bool {
        self.recording == SpineToolOutputRecording::Skip
    }

    pub(crate) fn records_raw_only_durable_without_emission(&self) -> bool {
        self.recording == SpineToolOutputRecording::RawOnlyDurableWithoutEmission
    }

    pub(crate) fn records_without_spine_observe(&self) -> bool {
        self.recording == SpineToolOutputRecording::WithoutSpineObserve
    }

    pub(crate) fn take_deferred_tree_update(&mut self) -> Option<SpineTreeUpdateEvent> {
        self.deferred_tree_update.take()
    }

    pub(crate) fn has_deferred_tree_update(&self) -> bool {
        self.deferred_tree_update.is_some()
    }
}

impl SpineRootCompactPublish {
    fn new(publication: SpineRootCompactHistoryPublication) -> Self {
        Self { publication }
    }

    pub(crate) fn materialized_len(&self) -> usize {
        self.publication.materialized_len()
    }

    pub(crate) fn published_history_from_native_items(
        &self,
        native_items: &[ResponseItem],
    ) -> Vec<ResponseItem> {
        self.publication
            .published_history_from_native_items(native_items, Session::is_spine_fixed_prefix_item)
    }
}

#[cfg(test)]
fn tool_commit_from_host_outcome(outcome: SpineCompletedToolCallHostOutcome) -> SpineToolCommit {
    let (recording, deferred_tree_update) = outcome.into_test_parts();
    SpineToolCommit {
        recording,
        deferred_tree_update,
    }
}

struct SpinePreparedToolCallEvidence<'a> {
    response_item: &'a ResponseItem,
    toolcall_evidence: SpineToolcallCommitEvidence,
}

struct SpineToolCallHostRecording {
    response_already_recorded: bool,
    response_recorded_inside_reduce: bool,
    history_before_recorded_output: Option<crate::context_manager::ContextManager>,
}

struct SpineCompletedToolCallOutputAnchor {
    raw_ordinals: Vec<Option<u64>>,
    context_start: usize,
    already_recorded: bool,
    recorded_inside_reduce: bool,
    history_before_recorded_output: Option<crate::context_manager::ContextManager>,
}

struct CompletedSpineToolCall<'a> {
    evidence: SpinePreparedToolCallEvidence<'a>,
    host_recording: SpineToolCallHostRecording,
}

struct SpineToolcallCommitAttemptInput<'a> {
    tool_resp_item: &'a ResponseItem,
    expected_history: Vec<ResponseItem>,
    provider_input_tokens: SpineToolcallCommitProviderInputTokens,
    toolcall_evidence: SpineToolcallCommitEvidence,
    tool_resp_already_recorded: bool,
    raw_items: &'a [Option<ResponseItem>],
}

enum SpineToolcallCommitHostControl {
    Done(SpineHostEffects),
    Retry,
    NoSpineCommit,
    FailClosed {
        reason: &'static str,
        error: SpineError,
    },
    AbortPending {
        reason: &'static str,
        error: SpineError,
    },
}

enum SpineToolcallCommitLoopControl {
    Done(SpineHostEffects),
    Retry,
    NoSpineCommit,
}

enum SpineTrimRequest {
    Snip,
    SliceHead {
        head: usize,
    },
    SliceTail {
        tail: usize,
    },
    SliceAnchor {
        anchor: String,
        preceding: usize,
        following: usize,
    },
}

impl Session {
    pub(crate) async fn send_spine_tree_update(
        &self,
        turn_context: &TurnContext,
        snapshot: SpineTreeUpdateEvent,
    ) {
        self.send_event(turn_context, EventMsg::SpineTreeUpdate(snapshot))
            .await;
    }

    pub(crate) async fn on_toolcall(
        self: &Arc<Self>,
        turn_context: &Arc<TurnContext>,
        evidence: SpineToolCallEvidence<'_>,
    ) -> Result<(), SpineToolcallTurnError> {
        let host_items = evidence
            .host_items_to_record_before_hook()
            .map_err(|err| SpineToolcallTurnError::Terminal(err.to_string()))?;
        if let Some(items) = host_items {
            self.record_conversation_items_without_spine_observe(turn_context, items)
                .await
                .map_err(SpineToolcallTurnError::Codex)?;
        }
        self.commit_toolcall_evidence(turn_context, evidence)
            .await
            .map_err(|err| SpineToolcallTurnError::Terminal(err.to_string()))
    }

    async fn commit_toolcall_evidence(
        self: &Arc<Self>,
        turn_context: &Arc<TurnContext>,
        evidence: SpineToolCallEvidence<'_>,
    ) -> Result<(), SpineError> {
        let Some(spine_slot) = self.spine.as_ref() else {
            return Ok(());
        };
        let Some(completed) = self
            .prepare_completed_spine_toolcall(turn_context, spine_slot, evidence)
            .await?
        else {
            return Ok(());
        };
        let mut outcome = self
            .commit_completed_spine_toolcall(turn_context, completed)
            .await?;
        self.apply_completed_spine_toolcall_host_outcome(turn_context.as_ref(), &mut outcome)
            .await;
        Ok(())
    }

    async fn apply_completed_spine_toolcall_host_outcome(
        &self,
        turn_context: &TurnContext,
        outcome: &mut SpineCompletedToolCallHostOutcome,
    ) {
        self.apply_completed_spine_toolcall_post_commit_effects(turn_context, outcome)
            .await;
        if let Some(snapshot) = outcome.take_deferred_tree_update() {
            self.send_spine_tree_update(turn_context, snapshot).await;
        }
    }

    async fn apply_completed_spine_toolcall_post_commit_effects(
        &self,
        turn_context: &TurnContext,
        outcome: &mut SpineCompletedToolCallHostOutcome,
    ) {
        let post_commit_effects = outcome.take_post_commit_effects();
        outcome.set_deferred_tree_update(
            self.apply_spine_post_commit_effects(turn_context, post_commit_effects)
                .await,
        );
    }

    fn apply_spine_host_effects_to_locked_state(
        state: &mut crate::state::SessionState,
        effects: SpineHostEffects,
    ) -> Result<(), String> {
        let _ = effects.apply_history_updates_or_keep(|effect| {
            let history = state.clone_history();
            effect.apply_history_update_or_self(
                history.raw_items(),
                |range, replacement, reference| {
                    state
                        .replace_history_suffix(range, replacement, reference)
                        .map_err(|err| err.to_string())
                },
            )
        })?;
        Ok(())
    }

    pub(crate) async fn emit_initial_spine_tree_snapshot_if_needed(
        &self,
        turn_context: &TurnContext,
    ) -> Result<(), SpineError> {
        if !self.features.enabled(Feature::SpineJit) {
            return Ok(());
        }
        let Some(spine_slot) = self.spine.as_ref() else {
            return Ok(());
        };
        let snapshot = {
            let mut guard = spine_slot.lock().await;
            guard.take_initial_tree_snapshot()?
        };
        if let Some(snapshot) = snapshot {
            self.send_spine_tree_update(turn_context, snapshot).await;
        }
        Ok(())
    }

    async fn apply_spine_post_commit_effects(
        &self,
        turn_context: &TurnContext,
        effects: SpineHostEffects,
    ) -> Option<SpineTreeUpdateEvent> {
        let (immediate, deferred) = effects.into_tree_host_updates().into_parts();
        for snapshot in immediate {
            self.send_spine_tree_update(turn_context, snapshot).await;
        }
        deferred.into_iter().last()
    }

    pub(crate) async fn seed_spine_tree_snapshot_if_available(&self) -> Result<(), SpineError> {
        if !self.features.enabled(Feature::SpineJit) {
            return Ok(());
        }
        let Some(spine_slot) = self.spine.as_ref() else {
            return Ok(());
        };
        let token_info = self.token_usage_info().await;
        // Host UI projection only: seeding the TUI snapshot must not mutate
        // ContextManager, ParseStack, or sidecar state.
        let snapshot = {
            let guard = spine_slot.lock().await;
            let Some(projection) = guard.tree_snapshot_projection()? else {
                return Ok(());
            };
            build_annotated_tree_snapshot(projection, token_info.as_ref())?
        };
        self.send_event_raw(Event {
            id: INITIAL_SUBMIT_ID.to_string(),
            msg: EventMsg::SpineTreeUpdate(snapshot),
        })
        .await;
        Ok(())
    }

    pub(super) async fn on_init(&self) -> Result<(), SpineError> {
        let Some(spine_slot) = self.spine.as_ref() else {
            return Ok(());
        };
        let Some(rollout_path) = self
            .current_rollout_path()
            .await
            .map_err(|err| SpineError::InvalidStore(err.to_string()))?
        else {
            return Ok(());
        };
        let mut guard = spine_slot.lock().await;
        let _effects = hooks::on_init(
            &mut guard,
            SpineInitEvidence {
                rollout_path: &rollout_path,
            },
        )?;
        Ok(())
    }

    pub(super) async fn spine_tools_visible(&self) -> bool {
        let Some(spine_slot) = self.spine.as_ref() else {
            return false;
        };
        let guard = spine_slot.lock().await;
        guard.is_ready()
    }

    pub(crate) async fn apply_spine_trim_projection_if_available(&self) -> Result<(), SpineError> {
        let Some(spine_slot) = self.spine.as_ref() else {
            return Ok(());
        };
        let Some(needs_rollout_raw_items) = ({
            let guard = spine_slot.lock().await;
            guard.trim_projection_needs_rollout_raw_items()?
        }) else {
            return Ok(());
        };
        let projected = if needs_rollout_raw_items {
            let raw_items = self.spine_raw_items_from_rollout().await?;
            let Some(projected) = ({
                let guard = spine_slot.lock().await;
                guard.materialize_trim_projection_from_raw_items(&raw_items)?
            }) else {
                return Ok(());
            };
            projected
        } else {
            let history = self.clone_history().await;
            let guard = spine_slot.lock().await;
            let Some(projected) =
                guard.project_trim_projection_from_history(history.raw_items())?
            else {
                return Ok(());
            };
            projected
        };
        if projected.as_slice() != self.clone_history().await.raw_items() {
            self.replace_history(projected, self.reference_context_item().await)
                .await;
        }
        Ok(())
    }

    pub(super) async fn release_spine_runtime_for_shutdown(&self) {
        let Some(spine_slot) = self.spine.as_ref() else {
            return;
        };
        spine_slot.lock().await.release_runtime_for_shutdown();
    }

    pub(super) async fn release_spine_runtime_for_replay(&self) {
        let Some(spine_slot) = self.spine.as_ref() else {
            return;
        };
        spine_slot.lock().await.release_runtime_for_replay();
    }

    pub(super) async fn clone_spine_sidecar_for_fork(
        &self,
        boundary: &SpineCloneBoundary,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<(), SpineError> {
        let Some(spine_slot) = self.spine.as_ref() else {
            return Ok(());
        };
        let Some(target_rollout_path) = self
            .current_rollout_path()
            .await
            .map_err(|err| SpineError::InvalidStore(err.to_string()))?
        else {
            return Ok(());
        };
        spine_slot.lock().await.install_cloned_sidecar_for_fork(
            boundary,
            &target_rollout_path,
            raw_items,
        )
    }

    pub(super) async fn prepare_spine_replay_from_rollout_items(
        &self,
        raw_items: &[Option<ResponseItem>],
        rollback_cuts: &[usize],
        used_replacement_history: bool,
        base_replacement_history_boundary: Option<&ReplacementHistoryBoundary>,
        replacement_history_boundaries: &[ReplacementHistoryBoundary],
    ) -> Result<Option<PreparedSpineReplay>, SpineError> {
        let Some(_spine_slot) = self.spine.as_ref() else {
            return Ok(None);
        };
        if !self.features.enabled(Feature::SpineJit) {
            return Ok(None);
        }
        let Some(rollout_path) = self
            .current_rollout_path()
            .await
            .map_err(|err| SpineError::InvalidStore(err.to_string()))?
        else {
            return Ok(None);
        };
        let raw_len = u64::try_from(raw_items.len())
            .map_err(|_| SpineError::InvalidEvent("raw item count overflow".to_string()))?;
        self.release_spine_runtime_for_replay().await;
        let spine_slot = self.spine.as_ref().ok_or_else(|| {
            SpineError::InvalidStore("spine_jit replay requires Spine session state".to_string())
        })?;
        let prepared_runtime = {
            let guard = spine_slot.lock().await;
            guard.prepare_jit_replay_from_rollout_items(&rollout_path, raw_items, rollback_cuts)?
        };
        if prepared_runtime.runtime.is_none()
            && (used_replacement_history || raw_items.iter().any(Option::is_some))
        {
            return Err(SpineError::InvalidStore(
                "spine_jit resume requires Spine sidecar".to_string(),
            ));
        }
        if used_replacement_history {
            let store = SpineStore::for_rollout(&rollout_path)?;
            let raw_live = raw_items.iter().map(Option::is_some).collect::<Vec<_>>();
            let Some(base_boundary) = base_replacement_history_boundary else {
                return Err(SpineError::InvalidStore(
                    "spine_jit resume used replacement_history without rollout compact boundary proof"
                        .to_string(),
                ));
            };
            let base_variable_replacement_history =
                Self::variable_spine_items_for_root_compact(&base_boundary.replacement_history);
            store.validate_compact_checkpoint_for_boundary(
                &rollout_path,
                &raw_live,
                raw_items,
                base_boundary.raw_boundary,
                &base_variable_replacement_history,
            )?;
            validate_live_root_compacts_have_rollout_boundary_proofs(
                &prepared_runtime.live_root_compacts,
                replacement_history_boundaries,
                &store,
                &rollout_path,
                &raw_live,
                raw_items,
            )?;
        } else {
            validate_no_live_root_compacts_without_rollout_boundaries(
                &prepared_runtime.live_root_compacts,
            )?;
        }
        Ok(Some(PreparedSpineReplay {
            raw_len,
            runtime: prepared_runtime.runtime,
            materialized: prepared_runtime.materialized,
        }))
    }

    pub(super) async fn prepare_spine_trim_replay_from_rollout_items(
        &self,
        raw_len: u64,
        history: &[ResponseItem],
    ) -> Result<Option<PreparedSpineReplay>, SpineError> {
        let Some(_spine_slot) = self.spine.as_ref() else {
            return Ok(None);
        };
        if !self.features.enabled(Feature::SpineTrim) {
            return Ok(None);
        }
        let Some(rollout_path) = self
            .current_rollout_path()
            .await
            .map_err(|err| SpineError::InvalidStore(err.to_string()))?
        else {
            return Ok(None);
        };
        let Some((runtime, materialized)) =
            SpineSessionState::prepare_trim_replay_from_history(&rollout_path, raw_len, history)?
        else {
            return Ok(None);
        };
        Ok(Some(PreparedSpineReplay::new(
            raw_len,
            Some(runtime),
            Some(materialized),
        )))
    }

    pub(super) async fn apply_spine_replay(
        &self,
        replay: PreparedSpineReplay,
    ) -> Result<Option<Vec<ResponseItem>>, SpineError> {
        let Some(spine_slot) = self.spine.as_ref() else {
            return Ok(replay.materialized);
        };
        spine_slot
            .lock()
            .await
            .set_replayed(replay.raw_len, replay.runtime)?;
        Ok(replay.materialized)
    }

    pub(crate) fn variable_spine_items_for_root_compact(
        items: &[ResponseItem],
    ) -> Vec<ResponseItem> {
        items
            .iter()
            .filter(|item| !Self::is_spine_fixed_prefix_item(item))
            .cloned()
            .collect()
    }

    pub(super) async fn observe_spine_raw_items(&self, count: usize) -> Result<(), SpineError> {
        let Some(spine_slot) = self.spine.as_ref() else {
            return Ok(());
        };
        spine_slot.lock().await.observe_raw_items(count)
    }

    pub(super) async fn emit_spine_tree_snapshot_cache_only_if_available(&self) {
        if !self.features.enabled(Feature::SpineJit) {
            return;
        }
        let Some(spine_slot) = self.spine.as_ref() else {
            return;
        };
        let token_info = self.token_usage_info().await;
        let snapshot = {
            let guard = spine_slot.lock().await;
            if let Err(err) = guard.ensure_valid() {
                tracing::debug!("skipping Spine tree cache refresh: {err}");
                return;
            }
            match guard
                .tree_snapshot_projection()
                .and_then(|projection| match projection {
                    Some(projection) => {
                        build_annotated_tree_snapshot(projection, token_info.as_ref()).map(Some)
                    }
                    None => Ok(None),
                }) {
                Ok(Some(snapshot)) => snapshot,
                Ok(None) => return,
                Err(err) => {
                    tracing::debug!("failed to build Spine tree cache refresh snapshot: {err}");
                    return;
                }
            }
        };
        self.send_event_raw(Event {
            id: INITIAL_SUBMIT_ID.to_string(),
            msg: EventMsg::SpineTreeUpdate(snapshot),
        })
        .await;
    }

    pub(super) async fn ensure_spine_runtime_if_available(&self) -> Result<(), SpineError> {
        let Some(spine_slot) = self.spine.as_ref() else {
            return Ok(());
        };
        let Some(rollout_path) = self
            .current_rollout_path()
            .await
            .map_err(|err| SpineError::InvalidStore(err.to_string()))?
        else {
            return Ok(());
        };
        spine_slot.lock().await.ensure_runtime(&rollout_path)
    }

    pub(super) async fn invalidate_spine_runtime(&self, reason: String) {
        let Some(spine_slot) = self.spine.as_ref() else {
            return;
        };
        spine_slot.lock().await.invalidate(reason);
    }

    pub(crate) async fn abort_spine_pending_tool(&self, call_id: &str, reason: &str) -> bool {
        let Some(spine_slot) = self.spine.as_ref() else {
            return false;
        };
        let mut guard = spine_slot.lock().await;
        if guard.ensure_valid().is_err() {
            return false;
        }
        let aborted = guard.abort_pending_tool(call_id);
        if aborted {
            tracing::debug!(call_id, reason, "aborted pending Spine transition");
        }
        aborted
    }

    async fn fail_closed_spine_toolcall_commit(&self, call_id: &str, reason: impl Into<String>) {
        let reason = reason.into();
        self.invalidate_spine_runtime(format!("{reason} for call_id={call_id}"))
            .await;
    }

    pub(crate) async fn abort_stale_spine_pending(&self, reason: &str) -> Option<String> {
        let Some(spine_slot) = self.spine.as_ref() else {
            return None;
        };
        let mut guard = spine_slot.lock().await;
        if guard.ensure_valid().is_err() {
            return None;
        }
        let aborted = guard.abort_any_pending();
        if let Some(call_id) = aborted.as_deref() {
            tracing::debug!(call_id, reason, "aborted stale pending Spine transition");
        }
        aborted
    }

    pub(super) async fn observe_spine_context_items(
        &self,
        raw_ordinals: &[Option<u64>],
        items: &[ResponseItem],
        appends: &[ContextAppend],
    ) -> Result<(), SpineError> {
        let Some(spine_slot) = self.spine.as_ref() else {
            return Ok(());
        };
        spine_slot.lock().await.ensure_valid()?;
        let rollout_path = self
            .current_rollout_path()
            .await
            .map_err(|err| SpineError::InvalidStore(err.to_string()))?
            .ok_or_else(|| {
                SpineError::InvalidStore("spine_jit checkpoint requires rollout path".to_string())
            })?;
        let rollout_history = crate::rollout::RolloutRecorder::get_rollout_history(&rollout_path)
            .await
            .map_err(|err| SpineError::InvalidStore(err.to_string()))?;
        let raw_items = spine_raw_items_after_rollback(&rollout_history.get_rollout_items());
        let mut non_toolcall_msg_effects = SpineHostEffects::none();
        let mut tool_items = Vec::new();
        for append in appends {
            let (raw_ordinal, item) = context_append_raw_item(raw_ordinals, items, append)?;
            if Self::is_spine_context_observation_fixed_prefix_item(item) {
                continue;
            }
            if is_non_toolcall_msg(item) {
                let outcome = self
                    .on_non_toolcall_msg(SpineMessageEvidence {
                        rollout_path: &rollout_path,
                        raw_ordinal,
                        context_index: append.context_index,
                        item,
                        raw_items: &raw_items,
                    })
                    .await?;
                non_toolcall_msg_effects.extend(outcome);
            } else {
                tool_items.push(SpineObservedContextItem {
                    raw_ordinal,
                    context_index: append.context_index,
                    item,
                });
            }
        }
        if !tool_items.is_empty() {
            let mut guard = spine_slot.lock().await;
            guard.observe_toolcall_context_items(&tool_items, &raw_items)?;
        }
        let (non_toolcall_msg_effects, publish_materialized_history_after_batch) =
            non_toolcall_msg_effects.into_after_batch_materialized_history_request();
        self.apply_non_toolcall_msg_host_outcome(non_toolcall_msg_effects)
            .await
            .map_err(SpineError::Invariant)?;
        if publish_materialized_history_after_batch {
            let outcome = self
                .materialized_history_host_effects_if_no_pending_tool_request(&raw_items)
                .await?;
            self.apply_non_toolcall_msg_host_outcome(outcome)
                .await
                .map_err(SpineError::Invariant)?;
        }
        Ok(())
    }

    pub(crate) async fn on_non_toolcall_msg(
        &self,
        evidence: SpineMessageEvidence<'_>,
    ) -> Result<SpineHostEffects, SpineError> {
        let Some(spine_slot) = self.spine.as_ref() else {
            return Ok(SpineHostEffects::none());
        };
        let mut guard = spine_slot.lock().await;
        hooks::on_non_toolcall_msg(&mut guard, evidence)
    }

    async fn materialized_history_host_effects_if_no_pending_tool_request(
        &self,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<SpineHostEffects, SpineError> {
        let Some(spine_slot) = self.spine.as_ref() else {
            return Ok(SpineHostEffects::none());
        };
        let history = self.clone_history().await;
        let expected_history = history.raw_items().to_vec();
        let reference_context_item = history.reference_context_item();
        let guard = spine_slot.lock().await;
        guard.materialized_history_host_effects_if_no_pending_tool_request(
            raw_items,
            expected_history,
            reference_context_item,
        )
    }

    async fn apply_non_toolcall_msg_host_outcome(
        &self,
        effects: SpineHostEffects,
    ) -> Result<(), String> {
        let effects = {
            let mut state = self.state.lock().await;
            effects.apply_history_updates_or_keep(|effect| {
                let history = state.clone_history();
                effect.apply_history_update_or_self(
                    history.raw_items(),
                    |range, replacement, reference| {
                        state
                            .replace_history_suffix(range, replacement, reference)
                            .map_err(|err| err.to_string())
                    },
                )
            })?
        };
        let (immediate, deferred) = effects.into_tree_host_updates().into_parts();
        if !deferred.is_empty() {
            return Err("non-toolcall message hook cannot defer tree update delivery".to_string());
        }
        for snapshot in immediate {
            self.send_event_raw(Event {
                id: INITIAL_SUBMIT_ID.to_string(),
                msg: EventMsg::SpineTreeUpdate(snapshot),
            })
            .await;
        }
        Ok(())
    }

    async fn ensure_spine_runtime(&self) -> Result<&Mutex<SpineSessionState>, SpineError> {
        let Some(spine_slot) = self.spine.as_ref() else {
            return Err(SpineError::InvalidStore(
                "spine_jit is disabled or this session has no persisted rollout".to_string(),
            ));
        };
        let Some(rollout_path) = self
            .current_rollout_path()
            .await
            .map_err(|err| SpineError::InvalidStore(err.to_string()))?
        else {
            return Err(SpineError::InvalidStore(
                "spine_jit requires a persisted rollout".to_string(),
            ));
        };
        let mut guard = spine_slot.lock().await;
        guard.ensure_valid()?;
        guard.ensure_runtime(&rollout_path)?;
        drop(guard);
        Ok(spine_slot)
    }

    #[cfg(test)]
    pub(crate) async fn spine_tree(&self) -> Result<String, SpineError> {
        self.spine_tree_with_plan(None).await
    }

    pub(crate) async fn spine_tree_with_plan(
        &self,
        planned_nodes: Option<Vec<SpinePlannedNodeSnapshot>>,
    ) -> Result<String, SpineError> {
        let spine = self.ensure_spine_runtime().await?;
        let token_info = self.token_usage_info().await;
        let view = {
            let guard = spine.lock().await;
            let Some(projection) = guard.tree_snapshot_projection()? else {
                return Err(SpineError::InvalidStore(
                    "spine runtime missing after initialization".to_string(),
                ));
            };
            let annotations =
                build_spine_tree_context_annotations(&projection, token_info.as_ref());
            let rendered_tree = guard
                .render_tree_with_context_annotations(&annotations)?
                .ok_or_else(|| {
                    SpineError::InvalidStore(
                        "spine runtime missing after initialization".to_string(),
                    )
                })?;
            build_spine_tree_inside_view_from_projection(
                projection,
                rendered_tree,
                token_info.as_ref(),
            )
        };

        if let Some(planned_nodes) = planned_nodes {
            let planned_nodes = validate_planned_nodes(&view.snapshot, planned_nodes)?;
            let mut guard = self.spine_planned_nodes.lock().await;
            *guard = planned_nodes;
        } else {
            self.prune_spine_planned_nodes(&view.snapshot).await;
        }

        let planned_nodes = self.spine_planned_nodes.lock().await.clone();
        Ok(render_spine_tree_for_model_with_plan(
            view.rendered_tree,
            &planned_nodes,
        ))
    }

    pub(crate) async fn emit_spine_tree_snapshot(
        &self,
        turn_context: &TurnContext,
    ) -> Result<(), SpineError> {
        let spine = self.ensure_spine_runtime().await?;
        let token_info = self.token_usage_info().await;
        let mut snapshot = {
            let guard = spine.lock().await;
            let Some(projection) = guard.tree_snapshot_projection()? else {
                return Err(SpineError::InvalidStore(
                    "spine runtime missing after initialization".to_string(),
                ));
            };
            build_annotated_tree_snapshot(projection, token_info.as_ref())?
        };
        self.prune_spine_planned_nodes(&snapshot).await;
        snapshot.planned_nodes = self.spine_planned_nodes.lock().await.clone();
        self.send_spine_tree_update(turn_context, snapshot).await;
        Ok(())
    }

    async fn prune_spine_planned_nodes(&self, snapshot: &SpineTreeUpdateEvent) {
        let mut planned_nodes = self.spine_planned_nodes.lock().await;
        *planned_nodes = retain_still_valid_planned_nodes(snapshot, &planned_nodes);
    }

    #[cfg(test)]
    pub(crate) async fn test_seed_spine_open_control_request(
        &self,
        call_id: String,
        summary: String,
    ) -> Result<(), SpineError> {
        // TODO(spine-hook-refactor): replace direct runtime seeding with
        // test evidence passed through the unified on_toolcall hook.
        let spine = self.ensure_spine_runtime().await?;
        let mut guard = spine.lock().await;
        guard.ensure_valid()?;
        let Some(runtime) = guard.runtime_mut() else {
            return Err(SpineError::InvalidStore(
                "spine runtime missing after initialization".to_string(),
            ));
        };
        runtime.stage_open(call_id, summary)
    }

    #[cfg(test)]
    pub(crate) async fn test_seed_spine_close_control_request<M: IntoSpineNodeMemory>(
        &self,
        call_id: String,
        memory: M,
    ) -> Result<(), SpineError> {
        // TODO(spine-hook-refactor): replace direct runtime seeding with
        // test evidence passed through the unified on_toolcall hook.
        let spine = self.ensure_spine_runtime().await?;
        let mut guard = spine.lock().await;
        guard.ensure_valid()?;
        let Some(runtime) = guard.runtime_mut() else {
            return Err(SpineError::InvalidStore(
                "spine runtime missing after initialization".to_string(),
            ));
        };
        runtime.stage_close(call_id, memory)
    }

    #[cfg(test)]
    pub(crate) async fn test_seed_spine_next_control_request<M: IntoSpineNodeMemory>(
        &self,
        call_id: String,
        summary: String,
        memory: M,
    ) -> Result<(), SpineError> {
        // TODO(spine-hook-refactor): replace direct runtime seeding with
        // test evidence passed through the unified on_toolcall hook.
        let spine = self.ensure_spine_runtime().await?;
        let mut guard = spine.lock().await;
        guard.ensure_valid()?;
        let Some(runtime) = guard.runtime_mut() else {
            return Err(SpineError::InvalidStore(
                "spine runtime missing after initialization".to_string(),
            ));
        };
        runtime.stage_next(call_id, summary, memory)
    }

    pub(crate) async fn trim_spine_tool_response(
        &self,
        trim_id: String,
    ) -> Result<SpineTrimOutcome, SpineError> {
        self.apply_spine_trim_request(trim_id, SpineTrimRequest::Snip)
            .await
    }

    pub(crate) async fn slice_spine_tool_response_head(
        &self,
        trim_id: String,
        head: usize,
    ) -> Result<SpineTrimOutcome, SpineError> {
        self.apply_spine_trim_request(trim_id, SpineTrimRequest::SliceHead { head })
            .await
    }

    pub(crate) async fn slice_spine_tool_response_tail(
        &self,
        trim_id: String,
        tail: usize,
    ) -> Result<SpineTrimOutcome, SpineError> {
        self.apply_spine_trim_request(trim_id, SpineTrimRequest::SliceTail { tail })
            .await
    }

    pub(crate) async fn slice_spine_tool_response_anchor(
        &self,
        trim_id: String,
        anchor: String,
        preceding: usize,
        following: usize,
    ) -> Result<SpineTrimOutcome, SpineError> {
        self.apply_spine_trim_request(
            trim_id,
            SpineTrimRequest::SliceAnchor {
                anchor,
                preceding,
                following,
            },
        )
        .await
    }

    async fn apply_spine_trim_request(
        &self,
        trim_id: String,
        request: SpineTrimRequest,
    ) -> Result<SpineTrimOutcome, SpineError> {
        match request {
            SpineTrimRequest::Snip => {
                let spine = self.ensure_spine_runtime().await?;
                let mut guard = spine.lock().await;
                guard.trim_tool_response(&trim_id)
            }
            SpineTrimRequest::SliceHead { head } => {
                let raw_items = self.spine_raw_items_from_rollout().await?;
                let spine = self.ensure_spine_runtime().await?;
                let mut guard = spine.lock().await;
                guard.slice_tool_response_head(&trim_id, head, &raw_items)
            }
            SpineTrimRequest::SliceTail { tail } => {
                let raw_items = self.spine_raw_items_from_rollout().await?;
                let spine = self.ensure_spine_runtime().await?;
                let mut guard = spine.lock().await;
                guard.slice_tool_response_tail(&trim_id, tail, &raw_items)
            }
            SpineTrimRequest::SliceAnchor {
                anchor,
                preceding,
                following,
            } => {
                let raw_items = self.spine_raw_items_from_rollout().await?;
                let spine = self.ensure_spine_runtime().await?;
                let mut guard = spine.lock().await;
                guard
                    .slice_tool_response_anchor(&trim_id, &anchor, preceding, following, &raw_items)
            }
        }
    }

    #[cfg(test)]
    pub(crate) async fn test_on_toolcall(
        self: &Arc<Self>,
        turn_context: &Arc<TurnContext>,
        evidence: SpineToolCallEvidence<'_>,
    ) -> Result<SpineToolCommit, SpineError> {
        let Some(spine_slot) = self.spine.as_ref() else {
            return Ok(tool_commit_from_host_outcome(
                SpineCompletedToolCallHostOutcome::no_spine_commit(),
            ));
        };
        let Some(completed) = self
            .prepare_completed_spine_toolcall(turn_context, spine_slot, evidence)
            .await?
        else {
            return Ok(tool_commit_from_host_outcome(
                SpineCompletedToolCallHostOutcome::no_spine_commit(),
            ));
        };
        let mut outcome = self
            .commit_completed_spine_toolcall(turn_context, completed)
            .await?;
        self.apply_completed_spine_toolcall_post_commit_effects(
            turn_context.as_ref(),
            &mut outcome,
        )
        .await;
        Ok(tool_commit_from_host_outcome(outcome))
    }

    async fn prepare_completed_spine_toolcall<'a>(
        self: &Arc<Self>,
        turn_context: &Arc<TurnContext>,
        spine_slot: &Mutex<SpineSessionState>,
        evidence: SpineToolCallEvidence<'a>,
    ) -> Result<Option<CompletedSpineToolCall<'a>>, SpineError> {
        let Some(output) = evidence.completed_output()? else {
            return Ok(None);
        };
        self.prepare_completed_spine_toolcall_output(turn_context, spine_slot, output)
            .await
    }

    async fn prepare_completed_spine_toolcall_output<'a>(
        self: &Arc<Self>,
        turn_context: &Arc<TurnContext>,
        spine_slot: &Mutex<SpineSessionState>,
        output: SpineCompletedToolCallOutputEvidence<'a>,
    ) -> Result<Option<CompletedSpineToolCall<'a>>, SpineError> {
        let Some(output_anchor) = self
            .record_completed_spine_toolcall_output_if_needed(turn_context, spine_slot, &output)
            .await?
        else {
            return Ok(None);
        };
        let Some(toolcall_evidence) = self
            .completed_spine_toolcall_commit_evidence(
                spine_slot,
                &output,
                output_anchor.raw_ordinals.as_slice(),
                output_anchor.context_start,
            )
            .await?
        else {
            return Ok(None);
        };
        Ok(Some(CompletedSpineToolCall {
            evidence: SpinePreparedToolCallEvidence {
                response_item: output.commit_output_item(),
                toolcall_evidence,
            },
            host_recording: SpineToolCallHostRecording {
                response_already_recorded: output_anchor.already_recorded,
                response_recorded_inside_reduce: output_anchor.recorded_inside_reduce,
                history_before_recorded_output: output_anchor.history_before_recorded_output,
            },
        }))
    }

    async fn record_completed_spine_toolcall_output_if_needed(
        self: &Arc<Self>,
        turn_context: &Arc<TurnContext>,
        spine_slot: &Mutex<SpineSessionState>,
        output: &SpineCompletedToolCallOutputEvidence<'_>,
    ) -> Result<Option<SpineCompletedToolCallOutputAnchor>, SpineError> {
        if let Some((call_id, item)) = output.single_output_requiring_optional_prerecord() {
            return self
                .record_single_spine_toolcall_output_if_needed(
                    turn_context,
                    spine_slot,
                    call_id,
                    item,
                )
                .await;
        }
        let Some(output_items) = output.output_group_to_record_before_commit() else {
            return Ok(None);
        };
        let output_raw_ordinals = {
            let guard = spine_slot.lock().await;
            guard
                .prepare_grouped_toolcall_output_recording(output_items)?
                .into_raw_ordinals()
        };
        let output_context_start = self.clone_history().await.raw_items().len();
        self.record_conversation_items_without_spine_observe(turn_context, output_items)
            .await
            .map_err(|err| {
                SpineError::Operation(format!(
                    "failed to record grouped Spine tool outputs before commit: {err}"
                ))
            })?;
        Ok(Some(SpineCompletedToolCallOutputAnchor {
            raw_ordinals: output_raw_ordinals,
            context_start: output_context_start,
            already_recorded: true,
            recorded_inside_reduce: true,
            history_before_recorded_output: None,
        }))
    }

    async fn record_single_spine_toolcall_output_if_needed(
        self: &Arc<Self>,
        turn_context: &Arc<TurnContext>,
        spine_slot: &Mutex<SpineSessionState>,
        call_id: &str,
        item: &ResponseItem,
    ) -> Result<Option<SpineCompletedToolCallOutputAnchor>, SpineError> {
        let mut recorded_output_inside_reduce = false;
        let mut history_before_recorded_output = None;
        let mut raw_len;
        let mut history_for_output_anchor;
        loop {
            history_for_output_anchor = self.clone_history().await;
            let history_items_for_output_anchor = history_for_output_anchor.raw_items();
            let raw_items = self.spine_raw_items_from_rollout_for_commit().await?;
            let recording_plan = {
                let guard = spine_slot.lock().await;
                let Some(recording_plan) =
                    guard.prepare_single_toolcall_output_recording(call_id, &raw_items)?
                else {
                    return Ok(None);
                };
                recording_plan
            };
            raw_len = recording_plan.raw_len();
            let tool_resp_already_recorded =
                history_items_for_output_anchor.last() == Some(item) && raw_len > 0;
            if tool_resp_already_recorded || recorded_output_inside_reduce {
                break;
            }
            if !recording_plan.prerecord_output_before_reduce() {
                break;
            }
            history_before_recorded_output = Some(history_for_output_anchor.clone());
            self.record_conversation_items_without_spine_observe(
                turn_context,
                std::slice::from_ref(item),
            )
            .await
            .map_err(|err| {
                SpineError::Operation(format!(
                    "failed to record Spine close-like raw output before reduce for call_id={call_id}: {err}"
                ))
            })?;
            recorded_output_inside_reduce = true;
        }
        let history_items_for_output_anchor = history_for_output_anchor.raw_items();
        let tool_resp_already_recorded =
            history_items_for_output_anchor.last() == Some(item) && raw_len > 0;
        let (tool_resp_raw_ordinal, tool_resp_context_index) = if tool_resp_already_recorded {
            (
                raw_len - 1,
                history_items_for_output_anchor
                    .len()
                    .checked_sub(1)
                    .ok_or_else(|| {
                        SpineError::Invariant(
                            "recorded tool output history length underflow".to_string(),
                        )
                    })?,
            )
        } else {
            (raw_len, history_items_for_output_anchor.len())
        };
        Ok(Some(SpineCompletedToolCallOutputAnchor {
            raw_ordinals: vec![Some(tool_resp_raw_ordinal)],
            context_start: tool_resp_context_index,
            already_recorded: tool_resp_already_recorded,
            recorded_inside_reduce: recorded_output_inside_reduce,
            history_before_recorded_output,
        }))
    }

    async fn completed_spine_toolcall_commit_evidence(
        &self,
        spine_slot: &Mutex<SpineSessionState>,
        output: &SpineCompletedToolCallOutputEvidence<'_>,
        output_raw_ordinals: &[Option<u64>],
        output_context_start: usize,
    ) -> Result<Option<SpineToolcallCommitEvidence>, SpineError> {
        let guard = spine_slot.lock().await;
        guard.ensure_valid()?;
        guard.completed_toolcall_commit_evidence_from_output(
            output,
            output_raw_ordinals,
            output_context_start,
        )
    }

    async fn commit_completed_spine_toolcall(
        self: &Arc<Self>,
        turn_context: &Arc<TurnContext>,
        toolcall: CompletedSpineToolCall<'_>,
    ) -> Result<SpineCompletedToolCallHostOutcome, SpineError> {
        let Some(spine_slot) = self.spine.as_ref() else {
            return Ok(SpineCompletedToolCallHostOutcome::no_spine_commit());
        };
        let call_id = toolcall.evidence.toolcall_evidence.call_id().to_string();
        let item = toolcall.evidence.response_item;
        let tool_resp_already_recorded = toolcall.host_recording.response_already_recorded;
        let recorded_output_inside_reduce = toolcall.host_recording.response_recorded_inside_reduce;
        let history_before_recorded_output = toolcall.host_recording.history_before_recorded_output;
        let raw_items = self.spine_raw_items_from_rollout_for_commit().await?;
        let current_turn_token_info = self.current_turn_token_usage_info(turn_context).await;
        let current_turn_provider_input_tokens = current_turn_token_info
            .as_ref()
            .and_then(provider_input_context_tokens);
        let mut commit_host_loop = {
            let mut guard = spine_slot.lock().await;
            guard.ensure_valid()?;
            let effects = hooks::on_toolcall(
                &mut guard,
                &toolcall.evidence.toolcall_evidence,
                &raw_items,
                current_turn_provider_input_tokens,
                tool_resp_already_recorded,
                recorded_output_inside_reduce,
            )?;
            let (effects, commit_host_loop) = effects
                .into_toolcall_commit_loop()
                .map_err(SpineError::Invariant)?;
            if !effects.is_empty() {
                return Err(SpineError::Invariant(
                    "toolcall hook returned unsupported host effects".to_string(),
                ));
            }
            let Some(commit_host_loop) = commit_host_loop else {
                return Ok(SpineCompletedToolCallHostOutcome::no_spine_commit());
            };
            commit_host_loop
        };
        let history = self.clone_history().await;
        let expected_history = history.raw_items().to_vec();
        let provider_input_tokens =
            commit_host_loop.provider_input_tokens(current_turn_provider_input_tokens);
        let post_commit_effects = loop {
            let attempt_input = SpineToolcallCommitAttemptInput {
                tool_resp_item: item,
                expected_history: expected_history.clone(),
                provider_input_tokens,
                toolcall_evidence: toolcall.evidence.toolcall_evidence.clone(),
                tool_resp_already_recorded,
                raw_items: &raw_items,
            };
            let step = self.try_commit_spine_tool_output_once(
                spine_slot,
                &mut commit_host_loop,
                &call_id,
                attempt_input,
            );
            let step = match step {
                Ok(step) => step,
                Err(err) => {
                    if recorded_output_inside_reduce {
                        if let Some(history) = history_before_recorded_output.as_ref() {
                            self.replace_history(
                                history.raw_items().to_vec(),
                                history.reference_context_item(),
                            )
                            .await;
                        }
                    }
                    if err.should_invalidate_runtime() {
                        self.invalidate_spine_runtime(format!(
                            "failed to commit completed Spine toolcall [{:?}] for call_id={call_id}: {err}",
                            err.class()
                        ))
                        .await;
                    }
                    return Err(err);
                }
            };
            match self
                .apply_spine_toolcall_commit_host_step(&call_id, step)
                .await?
            {
                SpineToolcallCommitLoopControl::Done(effects) => break effects,
                SpineToolcallCommitLoopControl::NoSpineCommit => {
                    return Ok(SpineCompletedToolCallHostOutcome::no_spine_commit());
                }
                SpineToolcallCommitLoopControl::Retry => {
                    tokio::task::yield_now().await;
                    continue;
                }
            }
        };
        Ok(commit_host_loop.host_outcome(post_commit_effects))
    }

    async fn apply_spine_toolcall_commit_host_step(
        &self,
        call_id: &str,
        control: SpineToolcallCommitHostControl,
    ) -> Result<SpineToolcallCommitLoopControl, SpineError> {
        match control {
            SpineToolcallCommitHostControl::Done(effects) => {
                Ok(SpineToolcallCommitLoopControl::Done(effects))
            }
            SpineToolcallCommitHostControl::NoSpineCommit => {
                Ok(SpineToolcallCommitLoopControl::NoSpineCommit)
            }
            SpineToolcallCommitHostControl::Retry => Ok(SpineToolcallCommitLoopControl::Retry),
            SpineToolcallCommitHostControl::FailClosed { reason, error } => {
                self.fail_closed_spine_toolcall_commit(call_id, reason)
                    .await;
                Err(error)
            }
            SpineToolcallCommitHostControl::AbortPending { reason, error } => {
                self.abort_spine_pending_tool(call_id, reason).await;
                Err(error)
            }
        }
    }

    fn try_commit_spine_tool_output_once(
        &self,
        spine_slot: &Mutex<SpineSessionState>,
        commit_host_loop: &mut SpineToolcallCommitHostLoop,
        call_id: &str,
        input: SpineToolcallCommitAttemptInput<'_>,
    ) -> Result<SpineToolcallCommitHostControl, SpineError> {
        let Ok(mut guard) = spine_slot.try_lock() else {
            return commit_host_loop.host_lock_busy_control(
                call_id,
                SpineToolcallCommitHostControl::Done,
                || SpineToolcallCommitHostControl::Retry,
                || SpineToolcallCommitHostControl::NoSpineCommit,
                |reason, error| SpineToolcallCommitHostControl::FailClosed { reason, error },
                |reason, error| SpineToolcallCommitHostControl::AbortPending { reason, error },
            );
        };
        let Ok(mut state) = self.state.try_lock() else {
            return commit_host_loop.host_lock_busy_control(
                call_id,
                SpineToolcallCommitHostControl::Done,
                || SpineToolcallCommitHostControl::Retry,
                || SpineToolcallCommitHostControl::NoSpineCommit,
                |reason, error| SpineToolcallCommitHostControl::FailClosed { reason, error },
                |reason, error| SpineToolcallCommitHostControl::AbortPending { reason, error },
            );
        };
        let reference_context_item = state.reference_context_item();
        let history = state.clone_history();
        let token_info = state.token_info();
        let attempt = guard.attempt_completed_toolcall_commit_with_host_effects(
            input.toolcall_evidence,
            input.tool_resp_item,
            input.tool_resp_already_recorded,
            input.raw_items,
            history.raw_items(),
            input.expected_history,
            reference_context_item,
            input.provider_input_tokens.pre_compact(),
            input.provider_input_tokens.current_turn(),
            |host_effects| Self::apply_spine_host_effects_to_locked_state(&mut state, host_effects),
            |projection| {
                if let Some(projection) = projection {
                    Ok(Some(build_annotated_tree_snapshot(
                        projection,
                        token_info.as_ref(),
                    )?))
                } else {
                    Ok(None)
                }
            },
        )?;
        commit_host_loop.interpret_attempt_for_host_control(
            attempt,
            call_id,
            SpineToolcallCommitHostControl::Done,
            || SpineToolcallCommitHostControl::Retry,
            || SpineToolcallCommitHostControl::NoSpineCommit,
            |reason, error| SpineToolcallCommitHostControl::FailClosed { reason, error },
            |reason, error| SpineToolcallCommitHostControl::AbortPending { reason, error },
        )
    }

    async fn spine_raw_items_from_rollout_for_commit(
        &self,
    ) -> Result<Vec<Option<ResponseItem>>, SpineError> {
        self.spine_raw_items_from_rollout().await
    }

    async fn spine_raw_items_from_rollout(&self) -> Result<Vec<Option<ResponseItem>>, SpineError> {
        self.ensure_rollout_materialized().await;
        self.flush_rollout()
            .await
            .map_err(|err| SpineError::InvalidStore(err.to_string()))?;
        let rollout_path = self
            .current_rollout_path()
            .await
            .map_err(|err| SpineError::InvalidStore(err.to_string()))?
            .ok_or_else(|| {
                SpineError::InvalidStore("spine raw trace lookup requires rollout path".to_string())
            })?;
        let rollout_history = crate::rollout::RolloutRecorder::get_rollout_history(&rollout_path)
            .await
            .map_err(|err| SpineError::InvalidStore(err.to_string()))?;
        Ok(spine_raw_items_after_rollback(
            &rollout_history.get_rollout_items(),
        ))
    }

    pub(crate) async fn is_spine_control_output_response_item(
        &self,
        item: &ResponseItem,
    ) -> Result<bool, SpineError> {
        let ResponseItem::FunctionCallOutput { call_id, .. } = item else {
            return Ok(false);
        };
        let Some(spine_slot) = self.spine.as_ref() else {
            return Ok(false);
        };
        let guard = spine_slot.lock().await;
        guard.ensure_valid()?;
        Ok(guard.is_control_output_call_id(call_id))
    }

    #[cfg(test)]
    pub(crate) async fn install_spine_root_compact(
        &self,
        body: String,
    ) -> Result<Option<(SpineRootCompactResult, SpineTreeUpdateEvent)>, SpineError> {
        let Some(spine_slot) = self.spine.as_ref() else {
            return Ok(None);
        };
        let Some(prepared) = self.prepare_spine_root_compact_impl(body).await? else {
            return Ok(None);
        };
        let result = prepared.result();
        let mut guard = spine_slot.lock().await;
        guard.ensure_valid()?;
        let snapshot =
            guard.apply_root_compact_after_history_publish(prepared, result.materialized.len())?;
        Ok(Some((result, snapshot)))
    }

    #[cfg(test)]
    async fn prepare_spine_root_compact_impl(
        &self,
        body: String,
    ) -> Result<Option<SpineRootCompactHostInstall>, SpineError> {
        let Some(spine_slot) = self.spine.as_ref() else {
            return Ok(None);
        };
        {
            let guard = spine_slot.lock().await;
            guard.ensure_valid()?;
            if !guard.is_ready() {
                return Ok(None);
            }
        }
        self.ensure_rollout_materialized().await;
        self.flush_rollout()
            .await
            .map_err(|err| SpineError::InvalidStore(err.to_string()))?;
        let rollout_path = self
            .current_rollout_path()
            .await
            .map_err(|err| SpineError::InvalidStore(err.to_string()))?
            .ok_or_else(|| {
                SpineError::InvalidStore("spine_jit root compact requires rollout path".to_string())
            })?;
        let history = crate::rollout::RolloutRecorder::get_rollout_history(&rollout_path)
            .await
            .map_err(|err| SpineError::InvalidStore(err.to_string()))?;
        let raw_items = spine_raw_items_after_rollback(&history.get_rollout_items());
        let close_provider_input_tokens = self
            .token_usage_info()
            .await
            .and_then(|info| provider_input_context_tokens(&info));
        let mut guard = spine_slot.lock().await;
        guard
            .prepare_native_root_compact_apply_with_checkpoint(
                &rollout_path,
                body,
                &raw_items,
                close_provider_input_tokens,
            )
            .map(Some)
    }

    pub(crate) async fn on_compact(
        &self,
        evidence: SpineCompactEvidence<'_>,
    ) -> CodexResult<Option<SpineRootCompactPublish>> {
        let effects = self
            .prepare_spine_root_compact_from_native_history(evidence)
            .await
            .map_err(|err| CodexErr::SpineTerminalFailure {
                operation: "install Spine root compact".to_string(),
                reason: err.to_string(),
            })?;
        let (effects, publication) =
            effects
                .into_root_compact_history_publication()
                .map_err(|reason| CodexErr::SpineTerminalFailure {
                    operation: "install Spine root compact".to_string(),
                    reason,
                })?;
        if !effects.is_empty() {
            return Err(CodexErr::SpineTerminalFailure {
                operation: "install Spine root compact".to_string(),
                reason: "compact hook returned unsupported host effects".to_string(),
            });
        }
        let Some(publication) = publication else {
            return Ok(None);
        };
        Ok(Some(SpineRootCompactPublish::new(publication)))
    }

    async fn prepare_spine_root_compact_from_native_history(
        &self,
        evidence: SpineCompactEvidence<'_>,
    ) -> Result<SpineHostEffects, SpineError> {
        let Some(spine_slot) = self.spine.as_ref() else {
            return Ok(SpineHostEffects::none());
        };
        self.ensure_rollout_materialized().await;
        self.flush_rollout()
            .await
            .map_err(|err| SpineError::InvalidStore(err.to_string()))?;
        let rollout_path = self
            .current_rollout_path()
            .await
            .map_err(|err| SpineError::InvalidStore(err.to_string()))?
            .ok_or_else(|| {
                SpineError::InvalidStore("spine_jit root compact requires rollout path".to_string())
            })?;
        let history = crate::rollout::RolloutRecorder::get_rollout_history(&rollout_path)
            .await
            .map_err(|err| SpineError::InvalidStore(err.to_string()))?;
        let raw_items = spine_raw_items_after_rollback(&history.get_rollout_items());
        let close_provider_input_tokens = self
            .token_usage_info()
            .await
            .and_then(|info| provider_input_context_tokens(&info));
        let mut guard = spine_slot.lock().await;
        hooks::on_compact(
            &mut guard,
            &rollout_path,
            &raw_items,
            close_provider_input_tokens,
            evidence,
        )
    }

    pub(crate) async fn finalize_spine_root_compact_after_history_publish(
        &self,
        prepared: SpineRootCompactPublish,
        published_history_len: usize,
    ) -> CodexResult<SpineRootCompactHostOutcome> {
        let Some(spine_slot) = self.spine.as_ref() else {
            return Err(CodexErr::SpineTerminalFailure {
                operation: "install Spine root compact".to_string(),
                reason: "spine runtime missing before root compact PS install".to_string(),
            });
        };
        let mut guard = spine_slot.lock().await;
        if prepared.materialized_len() != published_history_len {
            return Err(CodexErr::SpineTerminalFailure {
                operation: "install Spine root compact".to_string(),
                reason: format!(
                    "published history length {published_history_len} does not match prepared Spine root compact materialized length {}",
                    prepared.materialized_len()
                ),
            });
        }
        guard
            .take_pending_root_compact_host_outcome_after_history_publish(published_history_len)
            .map_err(|err| CodexErr::SpineTerminalFailure {
                operation: "install Spine root compact".to_string(),
                reason: err.to_string(),
            })
    }
}

fn context_append_raw_item<'a>(
    raw_ordinals: &[Option<u64>],
    items: &'a [ResponseItem],
    append: &ContextAppend,
) -> Result<(u64, &'a ResponseItem), SpineError> {
    let raw_ordinal = raw_ordinals
        .get(append.input_index)
        .copied()
        .flatten()
        .ok_or_else(|| {
            SpineError::InvalidEvent("context append has no persisted raw ordinal".to_string())
        })?;
    let item = items.get(append.input_index).ok_or_else(|| {
        SpineError::InvalidEvent("context append input index outside items".to_string())
    })?;
    Ok((raw_ordinal, item))
}

fn validate_live_root_compacts_have_rollout_boundary_proofs(
    live_root_compacts: &[LiveRootCompact],
    replacement_history_boundaries: &[ReplacementHistoryBoundary],
    store: &SpineStore,
    rollout_path: &Path,
    raw_live: &[bool],
    raw_items: &[Option<ResponseItem>],
) -> Result<(), SpineError> {
    for compact in live_root_compacts {
        if prove_live_root_compact_with_rollout_boundary(
            *compact,
            replacement_history_boundaries,
            store,
            rollout_path,
            raw_live,
            raw_items,
        )?
        .is_none()
        {
            return Err(SpineError::InvalidStore(format!(
                "spine_jit root compact sidecar is missing rollout compact boundary at raw boundary {} token_seq {}",
                compact.raw_boundary, compact.token_seq
            )));
        }
    }
    Ok(())
}

fn prove_live_root_compact_with_rollout_boundary(
    compact: LiveRootCompact,
    replacement_history_boundaries: &[ReplacementHistoryBoundary],
    store: &SpineStore,
    rollout_path: &Path,
    raw_live: &[bool],
    raw_items: &[Option<ResponseItem>],
) -> Result<Option<()>, SpineError> {
    let mut saw_same_boundary = false;
    for boundary in replacement_history_boundaries
        .iter()
        .filter(|boundary| boundary.raw_boundary == compact.raw_boundary)
    {
        saw_same_boundary = true;
        let checkpoint_token_seq = store.validate_compact_checkpoint_for_boundary(
            rollout_path,
            raw_live,
            raw_items,
            boundary.raw_boundary,
            &Session::variable_spine_items_for_root_compact(&boundary.replacement_history),
        )?;
        if checkpoint_token_seq.checked_sub(1) == Some(compact.token_seq) {
            return Ok(Some(()));
        }
    }
    if saw_same_boundary {
        return Err(SpineError::InvalidStore(format!(
            "spine compact checkpoint token_seq does not match live RootCompact at raw boundary {} token_seq {}",
            compact.raw_boundary, compact.token_seq
        )));
    }
    Ok(None)
}

fn validate_no_live_root_compacts_without_rollout_boundaries(
    live_root_compacts: &[LiveRootCompact],
) -> Result<(), SpineError> {
    if let Some(compact) = live_root_compacts.first() {
        return Err(SpineError::InvalidStore(format!(
            "spine_jit root compact sidecar is missing rollout compact boundary at raw boundary {} token_seq {}",
            compact.raw_boundary, compact.token_seq
        )));
    }
    Ok(())
}

fn build_annotated_tree_snapshot(
    projection: (
        SpineTreeUpdateEvent,
        Vec<crate::spine::SpineOpenNodeContextProjection>,
    ),
    token_info: Option<&TokenUsageInfo>,
) -> Result<SpineTreeUpdateEvent, SpineError> {
    let (snapshot, open_node_projections) = projection;
    Ok(annotate_spine_tree_snapshot(
        snapshot,
        token_info,
        &open_node_projections,
    ))
}

fn render_spine_tree_for_model_with_plan(
    mut rendered_tree: String,
    planned_nodes: &[SpinePlannedNodeSnapshot],
) -> String {
    if planned_nodes.is_empty() {
        return rendered_tree;
    }
    rendered_tree.push_str("\n\nPlanned future nodes:");
    let planned_by_parent = planned_nodes_by_parent(planned_nodes);
    append_planned_nodes_for_model(&mut rendered_tree, &planned_by_parent, None, 0);
    rendered_tree
}

fn append_planned_nodes_for_model(
    rendered_tree: &mut String,
    planned_by_parent: &BTreeMap<Option<String>, Vec<&SpinePlannedNodeSnapshot>>,
    parent_id: Option<&str>,
    depth: usize,
) {
    let key = parent_id.map(str::to_string);
    let Some(nodes) = planned_by_parent.get(&key) else {
        return;
    };
    for node in nodes {
        rendered_tree.push('\n');
        rendered_tree.push_str(&"  ".repeat(depth));
        rendered_tree.push_str("[planned] ");
        rendered_tree.push_str(&node.node_id);
        rendered_tree.push(' ');
        rendered_tree.push_str(node.summary.trim());
        append_planned_nodes_for_model(
            rendered_tree,
            planned_by_parent,
            Some(&node.node_id),
            depth + 1,
        );
    }
}

fn planned_nodes_by_parent(
    planned_nodes: &[SpinePlannedNodeSnapshot],
) -> BTreeMap<Option<String>, Vec<&SpinePlannedNodeSnapshot>> {
    let planned_ids = planned_nodes
        .iter()
        .map(|node| node.node_id.as_str())
        .collect::<BTreeSet<_>>();
    let mut out: BTreeMap<Option<String>, Vec<&SpinePlannedNodeSnapshot>> = BTreeMap::new();
    for node in planned_nodes {
        let parent = node
            .parent_id
            .as_deref()
            .filter(|parent_id| planned_ids.contains(parent_id))
            .map(str::to_string);
        out.entry(parent).or_default().push(node);
    }
    out
}

fn retain_still_valid_planned_nodes(
    snapshot: &SpineTreeUpdateEvent,
    planned_nodes: &[SpinePlannedNodeSnapshot],
) -> Vec<SpinePlannedNodeSnapshot> {
    validate_planned_nodes(snapshot, planned_nodes.to_vec()).unwrap_or_default()
}

fn validate_planned_nodes(
    snapshot: &SpineTreeUpdateEvent,
    planned_nodes: Vec<SpinePlannedNodeSnapshot>,
) -> Result<Vec<SpinePlannedNodeSnapshot>, SpineError> {
    let committed_ids = snapshot
        .nodes
        .iter()
        .map(|node| node.node_id.as_str())
        .collect::<BTreeSet<_>>();
    let committed_nodes = snapshot
        .nodes
        .iter()
        .map(|node| (node.node_id.as_str(), node))
        .collect::<BTreeMap<_, _>>();
    let active = parse_spine_node_path(&snapshot.active_node_id)?;
    let active_parent = parent_path(&active);
    let active_index = *active
        .last()
        .ok_or_else(|| SpineError::InvalidEvent("active Spine node id is empty".to_string()))?;

    let max_committed_child_by_parent = max_committed_child_by_parent(&snapshot.nodes)?;
    let mut planned_ids = BTreeSet::new();
    let mut parsed_planned = BTreeMap::new();
    for node in &planned_nodes {
        if node.summary.trim().is_empty() {
            return Err(SpineError::InvalidEvent(format!(
                "planned Spine node {} requires a non-empty summary",
                node.node_id
            )));
        }
        let path = parse_spine_node_path(&node.node_id)?;
        if committed_ids.contains(node.node_id.as_str()) {
            return Err(SpineError::InvalidEvent(format!(
                "planned Spine node {} already exists in the committed tree",
                node.node_id
            )));
        }
        if !planned_ids.insert(node.node_id.as_str()) {
            return Err(SpineError::InvalidEvent(format!(
                "duplicate planned Spine node id {}",
                node.node_id
            )));
        }
        parsed_planned.insert(node.node_id.as_str(), path);
    }

    for node in &planned_nodes {
        let path = parsed_planned
            .get(node.node_id.as_str())
            .expect("planned path was parsed");
        let parent = parent_path(path);
        let parent_id = parent.as_ref().map(|parent| path_to_string(parent));
        if node.parent_id != parent_id {
            return Err(SpineError::InvalidEvent(format!(
                "planned Spine node {} parent_id must be {:?}",
                node.node_id, parent_id
            )));
        }
        let index = *path.last().ok_or_else(|| {
            SpineError::InvalidEvent("planned Spine node id is empty".to_string())
        })?;
        if planned_parent_contains(&parsed_planned, parent.as_deref()) {
            continue;
        }

        if parent.as_deref() == Some(active.as_slice()) {
            let max_existing = max_committed_child_by_parent
                .get(active.as_slice())
                .copied()
                .unwrap_or(0);
            if index > max_existing {
                continue;
            }
        }

        if parent.as_deref() == active_parent.as_deref() && index > active_index {
            continue;
        }

        let parent_key = parent_id.as_deref();
        if let Some(parent_key) = parent_key
            && committed_nodes.contains_key(parent_key)
        {
            return Err(SpineError::InvalidEvent(format!(
                "planned Spine node {} is not on the right side of the current active frontier",
                node.node_id
            )));
        }

        return Err(SpineError::InvalidEvent(format!(
            "planned Spine node {} must be a future child of the active node, a future sibling of the active node, or a descendant of another planned node",
            node.node_id
        )));
    }

    Ok(planned_nodes)
}

fn planned_parent_contains<'a>(
    parsed_planned: &BTreeMap<&'a str, Vec<u32>>,
    parent: Option<&[u32]>,
) -> bool {
    let Some(parent) = parent else {
        return false;
    };
    parsed_planned
        .values()
        .any(|path| path.as_slice() == parent)
}

fn max_committed_child_by_parent(
    nodes: &[SpineTreeNodeSnapshot],
) -> Result<BTreeMap<Vec<u32>, u32>, SpineError> {
    let mut out: BTreeMap<Vec<u32>, u32> = BTreeMap::new();
    for node in nodes {
        let path = parse_spine_node_path(&node.node_id)?;
        let Some(parent) = parent_path(&path) else {
            continue;
        };
        let Some(index) = path.last().copied() else {
            continue;
        };
        out.entry(parent)
            .and_modify(|existing| *existing = (*existing).max(index))
            .or_insert(index);
    }
    Ok(out)
}

fn parse_spine_node_path(node_id: &str) -> Result<Vec<u32>, SpineError> {
    let mut path = Vec::new();
    for part in node_id.split('.') {
        if part.is_empty() {
            return Err(SpineError::InvalidEvent(format!(
                "malformed Spine node id {node_id:?}"
            )));
        }
        let index = part.parse::<u32>().map_err(|_| {
            SpineError::InvalidEvent(format!("malformed Spine node id {node_id:?}"))
        })?;
        if index == 0 {
            return Err(SpineError::InvalidEvent(format!(
                "malformed Spine node id {node_id:?}: indexes are 1-based"
            )));
        }
        path.push(index);
    }
    if path.is_empty() {
        return Err(SpineError::InvalidEvent(
            "malformed empty Spine node id".to_string(),
        ));
    }
    Ok(path)
}

fn parent_path(path: &[u32]) -> Option<Vec<u32>> {
    (path.len() > 1).then(|| path[..path.len() - 1].to_vec())
}

fn path_to_string(path: &[u32]) -> String {
    path.iter()
        .map(|part| part.to_string())
        .collect::<Vec<_>>()
        .join(".")
}

fn provider_input_context_tokens(current: &TokenUsageInfo) -> Option<i64> {
    let input_tokens = current.last_token_usage.input_tokens;
    (input_tokens > 0).then_some(input_tokens)
}

impl Session {
    async fn current_turn_token_usage_info(
        &self,
        turn_context: &TurnContext,
    ) -> Option<TokenUsageInfo> {
        let current = self.token_usage_info().await?;
        let Some(turn_state) = self.turn_state_for_sub_id(&turn_context.sub_id).await else {
            let total_tokens = current.total_token_usage.total_tokens;
            let last_tokens = current.last_token_usage.total_tokens;
            return (total_tokens > 0 || last_tokens > 0).then_some(current);
        };
        let token_usage_at_turn_start = {
            let turn_state = turn_state.lock().await;
            turn_state.token_usage_at_turn_start.clone()
        };
        let total_tokens = current.total_token_usage.total_tokens;
        let last_tokens = current.last_token_usage.total_tokens;
        let turn_started_from_zero = token_usage_at_turn_start.total_tokens == 0;
        let has_fresh_turn_usage = total_tokens > token_usage_at_turn_start.total_tokens
            || (turn_started_from_zero && last_tokens > 0);
        has_fresh_turn_usage.then_some(current)
    }
}

pub(super) fn assign_spine_raw_ordinals(
    raw_start: u64,
    items: &[ResponseItem],
) -> Result<(Vec<Option<u64>>, usize), SpineError> {
    let mut next = raw_start;
    let mut ordinals = Vec::with_capacity(items.len());
    for item in items {
        if should_persist_response_item(item) {
            ordinals.push(Some(next));
            next = next
                .checked_add(1)
                .ok_or_else(|| SpineError::InvalidEvent("raw ordinal overflow".to_string()))?;
        } else {
            ordinals.push(None);
        }
    }
    let count = next
        .checked_sub(raw_start)
        .ok_or_else(|| SpineError::InvalidEvent("raw ordinal underflow".to_string()))?;
    Ok((
        ordinals,
        usize::try_from(count)
            .map_err(|_| SpineError::InvalidEvent("raw item count overflow".to_string()))?,
    ))
}
