use super::*;
use crate::context_manager::ContextAppend;
use crate::session::rollout_reconstruction::ReplacementHistoryBoundary;
use crate::session::spine_tree_inside::build_spine_tree_context_annotations;
use crate::session::spine_tree_inside::build_spine_tree_inside_view_from_projection;
use crate::spine::TrimBodyUpdate;
use crate::spine::TrimResponseKind;
use crate::spine::bridge::CompletedSpineToolCall;
use crate::spine::bridge::CompletedToolCallHostOutcome;
use crate::spine::bridge::ForkCloneBoundary;
use crate::spine::bridge::LifecycleRuntime;
use crate::spine::bridge::RawObservationRuntime;
use crate::spine::bridge::ReplayRootCompactBoundary;
use crate::spine::bridge::ReplayRuntime;
use crate::spine::bridge::RootCompactHistoryPublication;
#[cfg(test)]
use crate::spine::bridge::TestNodeMemoryInput;
#[cfg(test)]
use crate::spine::bridge::TestRootCompactHostInstall;
#[cfg(test)]
use crate::spine::bridge::TestRootCompactResult;
#[cfg(test)]
use crate::spine::bridge::TestRuntime;
#[cfg(test)]
use crate::spine::bridge::TestToolOutputRecording;
use crate::spine::bridge::ToolcallHostAttempt;
use crate::spine::bridge::ToolcallHostCommitInput;
use crate::spine::bridge::ToolcallRuntime;
use crate::spine::bridge::TreeSnapshotProjection;
use crate::spine::bridge::TrimOutcome;
use crate::spine::bridge::TrimRequest;
use crate::spine::bridge::TrimRuntime;
use crate::spine::bridge::is_non_toolcall_msg;
use crate::spine::hooks;
use crate::spine::hooks::CompactEvidence;
use crate::spine::hooks::HostEffects;
use crate::spine::hooks::InitEvidence;
use crate::spine::hooks::MessageEvidence;
use crate::spine::hooks::ToolCallEvidence;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::TokenUsageInfo;
use codex_protocol::spine_tree::SpineTreeUpdateEvent;
use codex_rollout::should_persist_response_item;

pub(super) struct PreparedSpineReplay {
    replay: ReplayRuntime,
}

impl PreparedSpineReplay {
    pub(super) fn new(replay: ReplayRuntime) -> Self {
        Self { replay }
    }
}

#[derive(Debug)]
#[cfg(test)]
pub(crate) struct SpineToolCommit {
    recording: TestToolOutputRecording,
    deferred_tree_update: Option<SpineTreeUpdateEvent>,
}

pub(crate) enum SpineToolcallTurnError {
    Terminal(String),
}

impl std::fmt::Display for SpineToolcallTurnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Terminal(message) => f.write_str(message),
        }
    }
}

#[cfg(test)]
impl SpineToolCommit {
    pub(crate) fn recording(&self) -> TestToolOutputRecording {
        self.recording
    }

    pub(crate) fn skips_host_recording(&self) -> bool {
        self.recording == TestToolOutputRecording::Skip
    }

    pub(crate) fn records_raw_only_durable_without_emission(&self) -> bool {
        self.recording == TestToolOutputRecording::RawOnlyDurableWithoutEmission
    }

    pub(crate) fn records_without_spine_observe(&self) -> bool {
        self.recording == TestToolOutputRecording::WithoutSpineObserve
    }

    pub(crate) fn take_deferred_tree_update(&mut self) -> Option<SpineTreeUpdateEvent> {
        self.deferred_tree_update.take()
    }

    pub(crate) fn has_deferred_tree_update(&self) -> bool {
        self.deferred_tree_update.is_some()
    }
}

#[cfg(test)]
fn tool_commit_from_host_outcome(outcome: CompletedToolCallHostOutcome) -> SpineToolCommit {
    let (recording, deferred_tree_update) = outcome.into_test_parts();
    SpineToolCommit {
        recording,
        deferred_tree_update,
    }
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
        evidence: ToolCallEvidence<'_>,
    ) -> Result<(), SpineToolcallTurnError> {
        self.commit_toolcall_evidence(turn_context, evidence)
            .await
            .map_err(|err| SpineToolcallTurnError::Terminal(err.to_string()))
    }

    async fn commit_toolcall_evidence(
        self: &Arc<Self>,
        turn_context: &Arc<TurnContext>,
        evidence: ToolCallEvidence<'_>,
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
        outcome: &mut CompletedToolCallHostOutcome,
    ) {
        outcome
            .apply_post_commit_effects_and_emit(
                |effects| self.apply_spine_post_commit_effects(turn_context, effects),
                |snapshot| self.send_spine_tree_update(turn_context, snapshot),
            )
            .await;
    }

    #[cfg(test)]
    async fn apply_completed_spine_toolcall_post_commit_effects(
        &self,
        turn_context: &TurnContext,
        outcome: &mut CompletedToolCallHostOutcome,
    ) {
        outcome
            .apply_post_commit_effects_deferred(|effects| {
                self.apply_spine_post_commit_effects(turn_context, effects)
            })
            .await;
    }

    fn apply_spine_host_effects_to_locked_state(
        state: &mut crate::state::SessionState,
        effects: HostEffects,
    ) -> Result<(), String> {
        let effects = effects.apply_history_updates_or_keep(|effect| {
            let current_history = state.clone_history().raw_items().to_vec();
            let fixed_context_source = current_history.clone();
            effect.apply_history_update_or_self(
                &current_history,
                |range, replacement, reference| {
                    let replacement = if range.start == 0 {
                        Session::merge_fixed_context_with_spine_history(
                            fixed_context_source,
                            replacement,
                        )
                    } else {
                        replacement
                    };
                    state
                        .replace_history_suffix(range, replacement, reference)
                        .map_err(|err| err.to_string())
                },
            )
        })?;
        let _ = effects.apply_trim_body_updates_or_keep(|updates| {
            Self::apply_spine_trim_body_updates_to_locked_state(state, updates)
        })?;
        Ok(())
    }

    fn apply_spine_trim_body_updates_to_locked_state(
        state: &mut crate::state::SessionState,
        updates: Vec<TrimBodyUpdate>,
    ) -> Result<(), String> {
        if updates.is_empty() {
            return Ok(());
        }
        for update in updates {
            let history = state.clone_history();
            let Some((full_index, replacement)) =
                trim_body_update_replacement(history.raw_items(), &update)
                    .map_err(|err| err.to_string())?
            else {
                continue;
            };
            state.replace_history_item(full_index, replacement)?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn apply_spine_trim_body_updates_to_locked_state_for_test(
        state: &mut crate::state::SessionState,
        updates: Vec<TrimBodyUpdate>,
    ) -> Result<(), String> {
        Self::apply_spine_trim_body_updates_to_locked_state(state, updates)
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
            TreeSnapshotProjection::take_initial_snapshot(&mut guard)?
        };
        if let Some(snapshot) = snapshot {
            self.send_spine_tree_update(turn_context, snapshot).await;
        }
        Ok(())
    }

    async fn apply_spine_post_commit_effects(
        &self,
        turn_context: &TurnContext,
        effects: HostEffects,
    ) -> Option<SpineTreeUpdateEvent> {
        let effects = {
            let mut state = self.state.lock().await;
            match effects.apply_trim_body_updates_or_keep(|updates| {
                Self::apply_spine_trim_body_updates_to_locked_state(&mut state, updates)
            }) {
                Ok(effects) => effects,
                Err(reason) => {
                    drop(state);
                    self.invalidate_spine_runtime(format!(
                        "failed to apply Spine trim local body update: {reason}"
                    ))
                    .await;
                    return None;
                }
            }
        };
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
        // runtime state or sidecar state.
        let snapshot = {
            let guard = spine_slot.lock().await;
            let Some(projection) = TreeSnapshotProjection::from_state(&guard)? else {
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
        let _effects = hooks::on_init(&mut guard, InitEvidence::new(&rollout_path))?;
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
        let Some(jit_enabled) = ({
            let guard = spine_slot.lock().await;
            guard.trim_projection_needs_rollout_raw_items()?
        }) else {
            return Ok(());
        };
        if jit_enabled {
            return Ok(());
        }
        let raw_items = self.spine_raw_items_from_rollout().await?;
        let Some(updates) = ({
            let guard = spine_slot.lock().await;
            guard.current_trim_body_updates(&raw_items)?
        }) else {
            return Ok(());
        };
        if !updates.is_empty() {
            let mut state = self.state.lock().await;
            Self::apply_spine_trim_body_updates_to_locked_state(&mut state, updates)
                .map_err(SpineError::Invariant)?;
        }
        Ok(())
    }

    pub(super) async fn release_spine_runtime_for_shutdown(&self) {
        let Some(spine_slot) = self.spine.as_ref() else {
            return;
        };
        let mut guard = spine_slot.lock().await;
        guard.release_runtime_for_shutdown();
    }

    pub(super) async fn release_spine_runtime_for_replay(&self) {
        let Some(spine_slot) = self.spine.as_ref() else {
            return;
        };
        let mut guard = spine_slot.lock().await;
        guard.release_runtime_for_replay();
    }

    pub(super) async fn clone_spine_sidecar_for_fork(
        &self,
        boundary: &ForkCloneBoundary,
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
        let mut guard = spine_slot.lock().await;
        LifecycleRuntime::install_cloned_sidecar_for_fork(
            &mut guard,
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
            ReplayRuntime::prepare_jit_replay_from_rollout_items(
                &guard,
                &rollout_path,
                raw_len,
                raw_items,
                rollback_cuts,
            )?
        };
        if !prepared_runtime.has_runtime()
            && (used_replacement_history || raw_items.iter().any(Option::is_some))
        {
            return Err(SpineError::InvalidStore(
                "spine_jit resume requires Spine sidecar".to_string(),
            ));
        }
        if used_replacement_history {
            let raw_live = raw_items.iter().map(Option::is_some).collect::<Vec<_>>();
            let Some(base_boundary) = base_replacement_history_boundary else {
                return Err(SpineError::InvalidStore(
                    "spine_jit resume used replacement_history without rollout compact boundary proof"
                        .to_string(),
                ));
            };
            let base_variable_replacement_history =
                Self::variable_spine_items_for_root_compact(&base_boundary.replacement_history);
            let replacement_history_boundary_items = replacement_history_boundaries
                .iter()
                .map(|boundary| {
                    (
                        boundary.raw_boundary,
                        Self::variable_spine_items_for_root_compact(&boundary.replacement_history),
                    )
                })
                .collect::<Vec<_>>();
            let replacement_history_boundary_facts = replacement_history_boundary_items
                .iter()
                .map(
                    |(raw_boundary, variable_replacement_history)| ReplayRootCompactBoundary {
                        raw_boundary: *raw_boundary,
                        variable_replacement_history,
                    },
                )
                .collect::<Vec<_>>();
            prepared_runtime.validate_rollout_compact_boundaries(
                &rollout_path,
                &raw_live,
                raw_items,
                ReplayRootCompactBoundary {
                    raw_boundary: base_boundary.raw_boundary,
                    variable_replacement_history: &base_variable_replacement_history,
                },
                &replacement_history_boundary_facts,
            )?;
        } else {
            prepared_runtime.validate_no_rollout_compact_boundaries()?;
        }
        Ok(Some(PreparedSpineReplay::new(prepared_runtime)))
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
        let Some(replay) =
            ReplayRuntime::prepare_trim_replay_from_history(&rollout_path, raw_len, history)?
        else {
            return Ok(None);
        };
        Ok(Some(PreparedSpineReplay::new(replay)))
    }

    pub(super) async fn apply_spine_replay(
        &self,
        replay: PreparedSpineReplay,
    ) -> Result<Option<Vec<ResponseItem>>, SpineError> {
        let Some(spine_slot) = self.spine.as_ref() else {
            return Ok(replay.replay.into_variable_context());
        };
        let mut guard = spine_slot.lock().await;
        replay.replay.install(&mut *guard)
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
        let mut guard = spine_slot.lock().await;
        RawObservationRuntime::observe_raw_items(&mut guard, count)
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
            match TreeSnapshotProjection::from_state(&guard).and_then(|projection| match projection
            {
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
        let mut guard = spine_slot.lock().await;
        LifecycleRuntime::ensure_runtime(&mut guard, &rollout_path)
    }

    pub(super) async fn invalidate_spine_runtime(&self, reason: String) {
        let Some(spine_slot) = self.spine.as_ref() else {
            return;
        };
        let mut guard = spine_slot.lock().await;
        guard.invalidate(reason);
    }

    pub(crate) async fn abort_spine_pending_tool(&self, call_id: &str, reason: &str) -> bool {
        let Some(spine_slot) = self.spine.as_ref() else {
            return false;
        };
        let mut guard = spine_slot.lock().await;
        let Ok(aborted) = guard.abort_pending_tool(call_id) else {
            return false;
        };
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
        let Ok(aborted) = guard.abort_any_pending() else {
            return None;
        };
        if let Some(call_id) = aborted.as_deref() {
            tracing::debug!(call_id, reason, "aborted stale pending Spine transition");
        }
        aborted
    }

    pub(crate) async fn close_stale_spine_pending_as_aborted_toolcall(
        self: &Arc<Self>,
        turn_context: &Arc<TurnContext>,
        reason: &str,
    ) -> Result<Option<String>, SpineToolcallTurnError> {
        let Some(spine_slot) = self.spine.as_ref() else {
            return Ok(None);
        };
        let call_id = {
            let guard = spine_slot.lock().await;
            guard.pending_call_id().map_err(|err| {
                SpineToolcallTurnError::Terminal(format!(
                    "failed to inspect pending Spine toolcall before abort: {err}"
                ))
            })?
        };
        let Some(call_id) = call_id else {
            return Ok(None);
        };
        let response_item = ResponseItem::FunctionCallOutput {
            call_id: call_id.clone(),
            output: FunctionCallOutputPayload {
                body: FunctionCallOutputBody::Text(format!(
                    "SPINE_TOOL_USE_FAILED: {reason}. No Spine control action was applied. Retry with valid Spine tool arguments."
                )),
                success: Some(false),
            },
        };
        self.on_toolcall(turn_context, ToolCallEvidence::single(&response_item))
            .await?;
        tracing::debug!(
            call_id,
            reason,
            "closed pending Spine toolcall as aborted ordinary toolcall"
        );
        Ok(Some(call_id))
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
        {
            let guard = spine_slot.lock().await;
            RawObservationRuntime::ensure_observable_context(&guard)?;
        }
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
        let history = self.clone_history().await;
        let history_items = history.raw_items();
        let mut non_toolcall_msg_effects = HostEffects::none();
        let mut recorded_tool_outputs = Vec::<(String, u64, usize)>::new();
        for append in appends {
            let (raw_ordinal, item) = context_append_raw_item(raw_ordinals, items, append)?;
            if Self::is_spine_context_observation_fixed_prefix_item(item) {
                continue;
            }
            let context_index = Self::spine_mutable_context_index_for_full_history_index(
                history_items,
                append.context_index,
            )?;
            if is_non_toolcall_msg(item) {
                if !recorded_tool_outputs.is_empty() {
                    let updates = {
                        let mut guard = spine_slot.lock().await;
                        guard.observe_recorded_tool_output_group_as_completed_toolcall(
                            &recorded_tool_outputs,
                            &raw_items,
                        )?
                    };
                    if !updates.is_empty() {
                        let mut state = self.state.lock().await;
                        Self::apply_spine_trim_body_updates_to_locked_state(&mut state, updates)
                            .map_err(SpineError::Invariant)?;
                    }
                    recorded_tool_outputs.clear();
                }
                let outcome = self
                    .on_non_toolcall_msg(MessageEvidence::new(
                        &rollout_path,
                        raw_ordinal,
                        context_index,
                        item,
                        &raw_items,
                    ))
                    .await?;
                non_toolcall_msg_effects.extend(outcome);
            } else {
                {
                    let mut guard = spine_slot.lock().await;
                    RawObservationRuntime::observe_context_item(
                        &mut guard,
                        raw_ordinal,
                        context_index,
                        item,
                    )?;
                }
                if let Some(call_id) = tool_response_call_id_for_trim(item) {
                    recorded_tool_outputs.push((call_id.to_string(), raw_ordinal, context_index));
                }
            }
        }
        if !recorded_tool_outputs.is_empty() {
            let updates = {
                let mut guard = spine_slot.lock().await;
                guard.observe_recorded_tool_output_group_as_completed_toolcall(
                    &recorded_tool_outputs,
                    &raw_items,
                )?
            };
            if !updates.is_empty() {
                let mut state = self.state.lock().await;
                Self::apply_spine_trim_body_updates_to_locked_state(&mut state, updates)
                    .map_err(SpineError::Invariant)?;
            }
        }
        non_toolcall_msg_effects
            .apply_after_batch_variable_context_request_from_state(
                self.spine.as_ref(),
                &raw_items,
                SpineError::Invariant,
                |effects| async {
                    self.apply_non_toolcall_msg_host_outcome(effects)
                        .await
                        .map_err(SpineError::Invariant)
                },
                || async {
                    let history = self.clone_history().await;
                    (
                        history.raw_items().to_vec(),
                        history.reference_context_item(),
                    )
                },
                |effects| async {
                    self.apply_non_toolcall_msg_host_outcome(effects)
                        .await
                        .map_err(SpineError::Invariant)
                },
            )
            .await?;
        Ok(())
    }

    pub(crate) async fn on_non_toolcall_msg(
        &self,
        evidence: MessageEvidence<'_>,
    ) -> Result<HostEffects, SpineError> {
        let Some(spine_slot) = self.spine.as_ref() else {
            return Ok(HostEffects::none());
        };
        let mut guard = spine_slot.lock().await;
        hooks::on_non_toolcall_msg(&mut guard, evidence)
    }

    async fn apply_non_toolcall_msg_host_outcome(
        &self,
        effects: HostEffects,
    ) -> Result<(), String> {
        let effects = {
            let mut state = self.state.lock().await;
            effects.apply_history_updates_or_keep(|effect| {
                let current_history = state.clone_history().raw_items().to_vec();
                let fixed_context_source = current_history.clone();
                effect.apply_history_update_or_self(
                    &current_history,
                    |range, replacement, reference| {
                        let replacement = if range.start == 0 {
                            Session::merge_fixed_context_with_spine_history(
                                fixed_context_source.clone(),
                                replacement,
                            )
                        } else {
                            replacement
                        };
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
        LifecycleRuntime::ensure_runtime(&mut guard, &rollout_path)?;
        drop(guard);
        Ok(spine_slot)
    }

    pub(crate) async fn spine_tree(&self) -> Result<String, SpineError> {
        let spine = self.ensure_spine_runtime().await?;
        let token_info = self.token_usage_info().await;
        let view = {
            let guard = spine.lock().await;
            let Some(projection) = TreeSnapshotProjection::from_state(&guard)? else {
                return Err(SpineError::InvalidStore(
                    "spine runtime missing after initialization".to_string(),
                ));
            };
            let annotations =
                build_spine_tree_context_annotations(&projection, token_info.as_ref());
            let rendered_tree =
                TreeSnapshotProjection::render_tree_with_context_annotations(&guard, &annotations)?
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
        Ok(view.rendered_tree)
    }

    pub(crate) async fn emit_spine_tree_snapshot(
        &self,
        turn_context: &TurnContext,
    ) -> Result<(), SpineError> {
        let spine = self.ensure_spine_runtime().await?;
        let token_info = self.token_usage_info().await;
        let snapshot = {
            let guard = spine.lock().await;
            let Some(projection) = TreeSnapshotProjection::from_state(&guard)? else {
                return Err(SpineError::InvalidStore(
                    "spine runtime missing after initialization".to_string(),
                ));
            };
            build_annotated_tree_snapshot(projection, token_info.as_ref())?
        };
        self.send_spine_tree_update(turn_context, snapshot).await;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) async fn test_seed_spine_open_control_request(
        &self,
        call_id: String,
        summary: String,
    ) -> Result<(), SpineError> {
        let raw_items = self.spine_raw_items_from_rollout().await?;
        let spine = self.ensure_spine_runtime().await?;
        let mut guard = spine.lock().await;
        TestRuntime::seed_open_control_request(&mut guard, call_id, summary, &raw_items)
    }

    #[cfg(test)]
    pub(crate) async fn test_seed_spine_close_control_request<M: TestNodeMemoryInput>(
        &self,
        call_id: String,
        memory: M,
    ) -> Result<(), SpineError> {
        let raw_items = self.spine_raw_items_from_rollout().await?;
        let spine = self.ensure_spine_runtime().await?;
        let mut guard = spine.lock().await;
        TestRuntime::seed_close_control_request(&mut guard, call_id, memory, &raw_items)
    }

    #[cfg(test)]
    pub(crate) async fn test_seed_spine_next_control_request<M: TestNodeMemoryInput>(
        &self,
        call_id: String,
        summary: String,
        memory: M,
    ) -> Result<(), SpineError> {
        let raw_items = self.spine_raw_items_from_rollout().await?;
        let spine = self.ensure_spine_runtime().await?;
        let mut guard = spine.lock().await;
        TestRuntime::seed_next_control_request(&mut guard, call_id, summary, memory, &raw_items)
    }

    pub(crate) async fn trim_spine_tool_response(
        &self,
        trim_id: String,
    ) -> Result<TrimOutcome, SpineError> {
        self.apply_spine_trim_request(trim_id, TrimRequest::Snip)
            .await
    }

    pub(crate) async fn slice_spine_tool_response_head(
        &self,
        trim_id: String,
        head: usize,
    ) -> Result<TrimOutcome, SpineError> {
        self.apply_spine_trim_request(trim_id, TrimRequest::SliceHead { head })
            .await
    }

    pub(crate) async fn slice_spine_tool_response_tail(
        &self,
        trim_id: String,
        tail: usize,
    ) -> Result<TrimOutcome, SpineError> {
        self.apply_spine_trim_request(trim_id, TrimRequest::SliceTail { tail })
            .await
    }

    pub(crate) async fn slice_spine_tool_response_anchor(
        &self,
        trim_id: String,
        anchor: String,
        preceding: usize,
        following: usize,
    ) -> Result<TrimOutcome, SpineError> {
        self.apply_spine_trim_request(
            trim_id,
            TrimRequest::SliceAnchor {
                anchor: &anchor,
                preceding,
                following,
            },
        )
        .await
    }

    async fn apply_spine_trim_request(
        &self,
        trim_id: String,
        request: TrimRequest<'_>,
    ) -> Result<TrimOutcome, SpineError> {
        let raw_items = if request.needs_raw_items() {
            Some(self.spine_raw_items_from_rollout().await?)
        } else {
            None
        };
        let spine = self.ensure_spine_runtime().await?;
        let (outcome, updates) = {
            let mut guard = spine.lock().await;
            TrimRuntime::apply_tool_response_request(
                &mut guard,
                &trim_id,
                request,
                raw_items.as_deref(),
            )?
            .into_parts()
        };
        if !updates.is_empty() {
            let mut state = self.state.lock().await;
            Self::apply_spine_trim_body_updates_to_locked_state(&mut state, updates)
                .map_err(SpineError::Invariant)?;
        }
        Ok(outcome)
    }

    #[cfg(test)]
    pub(crate) async fn test_on_toolcall(
        self: &Arc<Self>,
        turn_context: &Arc<TurnContext>,
        evidence: impl Into<ToolCallEvidence<'_>>,
    ) -> Result<SpineToolCommit, SpineError> {
        let evidence = evidence.into();
        let Some(spine_slot) = self.spine.as_ref() else {
            return Ok(tool_commit_from_host_outcome(
                CompletedToolCallHostOutcome::no_spine_commit(),
            ));
        };
        let Some(completed) = self
            .prepare_completed_spine_toolcall(turn_context, spine_slot, evidence)
            .await?
        else {
            return Ok(tool_commit_from_host_outcome(
                CompletedToolCallHostOutcome::no_spine_commit(),
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
        evidence: ToolCallEvidence<'a>,
    ) -> Result<Option<CompletedSpineToolCall<'a>>, SpineError> {
        ToolcallRuntime::prepare_completed_toolcall_for_commit(
            &evidence,
            || async { self.clone_history().await },
            || async { self.spine_raw_items_from_rollout_for_commit().await },
            |call_id, raw_items| async move {
                let guard = spine_slot.lock().await;
                ToolcallRuntime::prepare_single_output_recording(&guard, &call_id, &raw_items)
            },
            |output_items| async move {
                let guard = spine_slot.lock().await;
                ToolcallRuntime::prepare_grouped_output_recording(&guard, &output_items)
            },
            Self::spine_mutable_context_index_for_full_history_boundary,
            |prevalidation| async move {
                let raw_items = self.spine_raw_items_from_rollout_for_commit().await?;
                let guard = spine_slot.lock().await;
                prevalidation.validate(&guard, &raw_items)
            },
            |items| async move {
                self.record_conversation_items_without_spine_observe(turn_context, &items)
                    .await
                    .map_err(|err| err.to_string())
            },
        )
        .await
    }

    async fn commit_completed_spine_toolcall(
        self: &Arc<Self>,
        turn_context: &Arc<TurnContext>,
        toolcall: CompletedSpineToolCall<'_>,
    ) -> Result<CompletedToolCallHostOutcome, SpineError> {
        let Some(spine_slot) = self.spine.as_ref() else {
            return Ok(CompletedToolCallHostOutcome::no_spine_commit());
        };
        let call_id = toolcall.call_id().to_string();
        let item = toolcall.response_item();
        let tool_resp_already_recorded = toolcall.response_already_recorded();
        let raw_items = self.spine_raw_items_from_rollout_for_commit().await?;
        let current_turn_token_info = self.current_turn_token_usage_info(turn_context).await;
        let current_turn_provider_input_tokens = current_turn_token_info
            .as_ref()
            .and_then(provider_input_context_tokens);
        let toolcall_host_effects = {
            let mut guard = spine_slot.lock().await;
            ToolcallRuntime::prepare_host_effects_for_commit(
                &mut guard,
                &toolcall,
                &raw_items,
                current_turn_provider_input_tokens,
            )?
        };
        let history = self.clone_history().await;
        let expected_history = history.raw_items().to_vec();
        let raw_items_ref = raw_items.as_slice();
        let outcome: Result<Option<CompletedToolCallHostOutcome>, SpineError> =
            toolcall_host_effects
                .apply_toolcall_host_commit(
                    &call_id,
                    current_turn_provider_input_tokens,
                    |attempt| {
                        let expected_history = expected_history.clone();
                        let raw_items = raw_items_ref;
                        async move {
                            let attempt_input = attempt.into_commit_input(
                                item,
                                tool_resp_already_recorded,
                                raw_items,
                                expected_history,
                            );
                            self.try_commit_spine_tool_output_once(spine_slot, attempt_input)
                        }
                    },
                    || async {
                        tokio::task::yield_now().await;
                    },
                    |reason| {
                        let call_id = call_id.clone();
                        async move {
                            self.fail_closed_spine_toolcall_commit(&call_id, reason)
                                .await;
                        }
                    },
                    |reason| {
                        let call_id = call_id.clone();
                        async move {
                            self.abort_spine_pending_tool(&call_id, reason).await;
                        }
                    },
                )
                .await;
        match outcome {
            Ok(Some(outcome)) => Ok(outcome),
            Ok(None) => return Ok(CompletedToolCallHostOutcome::no_spine_commit()),
            Err(err) => {
                if let Some(history) = toolcall.history_to_restore_on_commit_error() {
                    self.replace_history(
                        history.raw_items().to_vec(),
                        history.reference_context_item(),
                    )
                    .await;
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
        }
    }

    fn try_commit_spine_tool_output_once(
        &self,
        spine_slot: &Mutex<SpineSessionState>,
        input: ToolcallHostCommitInput<'_>,
    ) -> Result<ToolcallHostAttempt, SpineError> {
        let Ok(mut guard) = spine_slot.try_lock() else {
            return Ok(ToolcallHostAttempt::host_lock_busy());
        };
        let Ok(mut state) = self.state.try_lock() else {
            return Ok(ToolcallHostAttempt::host_lock_busy());
        };
        let reference_context_item = state.reference_context_item();
        let history = state.clone_history();
        let token_info = state.token_info();
        input.attempt_completed_toolcall_commit(
            &mut guard,
            history.raw_items(),
            reference_context_item,
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
        )
    }

    async fn spine_raw_items_from_rollout_for_commit(
        &self,
    ) -> Result<Vec<Option<ResponseItem>>, SpineError> {
        self.spine_raw_items_from_rollout().await
    }

    pub(crate) async fn spine_raw_items_from_rollout(
        &self,
    ) -> Result<Vec<Option<ResponseItem>>, SpineError> {
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
        guard.is_control_output_call_id(call_id)
    }

    #[cfg(test)]
    pub(crate) async fn install_spine_root_compact(
        &self,
        body: String,
    ) -> Result<Option<(TestRootCompactResult, SpineTreeUpdateEvent)>, SpineError> {
        let Some(spine_slot) = self.spine.as_ref() else {
            return Ok(None);
        };
        let Some(prepared) = self.prepare_spine_root_compact_impl(body).await? else {
            return Ok(None);
        };
        let publication = prepared.variable_context_publication_for_test();
        let mut guard = spine_slot.lock().await;
        let snapshot = TestRuntime::apply_root_compact_after_history_publish(
            &mut guard,
            prepared,
            publication.variable_context().len(),
        )?;
        Ok(Some((publication, snapshot)))
    }

    #[cfg(test)]
    async fn prepare_spine_root_compact_impl(
        &self,
        body: String,
    ) -> Result<Option<TestRootCompactHostInstall>, SpineError> {
        let Some(spine_slot) = self.spine.as_ref() else {
            return Ok(None);
        };
        {
            let guard = spine_slot.lock().await;
            if !TestRuntime::is_ready(&guard)? {
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
        TestRuntime::prepare_native_root_compact_apply_with_checkpoint(
            &mut guard,
            &rollout_path,
            body,
            &raw_items,
            close_provider_input_tokens,
        )
        .map(Some)
    }

    pub(crate) async fn on_compact(
        &self,
        compacted_history: &[ResponseItem],
    ) -> CodexResult<HostEffects> {
        self.prepare_spine_root_compact_from_native_history(compacted_history)
            .await
            .map_err(|err| CodexErr::SpineTerminalFailure {
                operation: "install Spine root compact".to_string(),
                reason: err.to_string(),
            })
    }

    async fn prepare_spine_root_compact_from_native_history(
        &self,
        compacted_history: &[ResponseItem],
    ) -> Result<HostEffects, SpineError> {
        let Some(spine_slot) = self.spine.as_ref() else {
            return Ok(HostEffects::none());
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
            CompactEvidence::new(
                &rollout_path,
                compacted_history,
                &raw_items,
                close_provider_input_tokens,
            ),
        )
    }

    pub(crate) async fn replace_compacted_history_with_spine_hooks(
        &self,
        _turn_context: &TurnContext,
        items: Vec<ResponseItem>,
        reference_context_item: Option<TurnContextItem>,
        compacted_item: CompactedItem,
        spine_root_compact_source: Option<Vec<ResponseItem>>,
    ) -> CodexResult<ReplaceCompactedHistoryOutcome> {
        let fallback_spine_root_compact_source;
        let spine_root_compact_source = match spine_root_compact_source.as_deref() {
            Some(source) => source,
            None => {
                fallback_spine_root_compact_source = items.clone();
                fallback_spine_root_compact_source.as_slice()
            }
        };
        let effects = self.on_compact(spine_root_compact_source).await?;
        let publish_reference_context_item = reference_context_item.clone();
        let spine_tree_snapshot = effects
            .apply_root_compact_history_publication(
                self.spine.as_ref(),
                items,
                Session::is_spine_fixed_prefix_item,
                |reason| CodexErr::SpineTerminalFailure {
                    operation: "install Spine root compact".to_string(),
                    reason,
                },
                |publication| {
                    let reference_context_item = publish_reference_context_item;
                    async move {
                        self.publish_spine_root_compact_history(
                            publication,
                            reference_context_item,
                            compacted_item,
                        )
                        .await
                    }
                },
                |reason| async move {
                    self.invalidate_spine_runtime(format!(
                        "failed to install Spine root compact after host history publication: {reason}"
                    ))
                    .await;
                    CodexErr::SpineTerminalFailure {
                        operation: "install Spine root compact".to_string(),
                        reason,
                    }
                },
                || async move { Ok(()) },
            )
            .await?;
        self.services.model_client.advance_window_generation();
        Ok(ReplaceCompactedHistoryOutcome {
            spine_tree_snapshot,
        })
    }

    async fn publish_spine_root_compact_history(
        &self,
        publication: RootCompactHistoryPublication,
        reference_context_item: Option<TurnContextItem>,
        compacted_item: CompactedItem,
    ) -> CodexResult<()> {
        let (published_items, compacted_item) =
            publication.into_compacted_rollout_item(compacted_item);
        let mut rollout_items = vec![RolloutItem::Compacted(compacted_item)];
        if let Some(turn_context_item) = reference_context_item.clone() {
            rollout_items.push(RolloutItem::TurnContext(turn_context_item));
        }
        if let Err(err) = self.try_persist_rollout_items(&rollout_items).await {
            let reason = err.to_string();
            self.invalidate_spine_runtime(format!(
                "failed to persist native compact rollout boundary after sidecar commit: {reason}"
            ))
            .await;
            return Err(CodexErr::SpineCompactCommitFailure {
                operation: "persist native compact rollout boundary".to_string(),
                reason,
            });
        }
        self.replace_history(published_items, reference_context_item)
            .await;
        Ok(())
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

fn trim_body_update_replacement(
    history: &[ResponseItem],
    update: &TrimBodyUpdate,
) -> Result<Option<(usize, ResponseItem)>, SpineError> {
    let Some((full_index, item)) = history
        .iter()
        .enumerate()
        .find(|(_, item)| trim_body_update_matches_item(update, item))
    else {
        return Ok(None);
    };
    let mut replacement = item.clone();
    replace_trim_body_exact(&mut replacement, update).map_err(|reason| {
        SpineError::Invariant(format!(
            "spine trim target {} failed local body update at raw_ordinal={} call_id={}: {reason}",
            update.trim_id, update.raw_ordinal, update.call_id
        ))
    })?;
    Ok(Some((full_index, replacement)))
}

fn trim_body_update_matches_item(update: &TrimBodyUpdate, item: &ResponseItem) -> bool {
    match (item, update.response_kind) {
        (
            ResponseItem::FunctionCallOutput { call_id, .. },
            TrimResponseKind::FunctionCallOutput,
        )
        | (
            ResponseItem::CustomToolCallOutput { call_id, .. },
            TrimResponseKind::CustomToolCallOutput,
        ) => call_id == &update.call_id,
        _ => false,
    }
}

fn replace_trim_body_exact(
    replacement: &mut ResponseItem,
    update: &TrimBodyUpdate,
) -> Result<(), &'static str> {
    match (replacement, update.response_kind) {
        (
            ResponseItem::FunctionCallOutput { call_id, output },
            TrimResponseKind::FunctionCallOutput,
        ) => {
            if call_id != &update.call_id {
                return Err("call_id mismatch");
            }
            output.body = FunctionCallOutputBody::Text(update.visible_body.clone());
        }
        (
            ResponseItem::CustomToolCallOutput {
                call_id, output, ..
            },
            TrimResponseKind::CustomToolCallOutput,
        ) => {
            if call_id != &update.call_id {
                return Err("call_id mismatch");
            }
            output.body = FunctionCallOutputBody::Text(update.visible_body.clone());
        }
        _ => return Err("response kind mismatch"),
    }
    Ok(())
}

fn tool_response_call_id_for_trim(item: &ResponseItem) -> Option<&str> {
    match item {
        ResponseItem::FunctionCallOutput { call_id, .. }
        | ResponseItem::CustomToolCallOutput { call_id, .. } => Some(call_id.as_str()),
        _ => None,
    }
}

fn build_annotated_tree_snapshot(
    projection: TreeSnapshotProjection,
    token_info: Option<&TokenUsageInfo>,
) -> Result<SpineTreeUpdateEvent, SpineError> {
    Ok(projection.into_annotated_snapshot(token_info.and_then(provider_input_context_tokens)))
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
