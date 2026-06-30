use super::*;
use crate::context_manager::ContextAppend;
use crate::function_tool::FunctionCallError;
use crate::session::rollout_reconstruction::ReplacementHistoryBoundary;
use crate::session::rollout_reconstruction::RolloutReconstruction;
#[cfg(test)]
use crate::spine::IntoSpineNodeMemory;
use crate::spine::SpineCloneBoundary;
#[cfg(test)]
use crate::spine::SpineRootCompactHostInstall;
#[cfg(test)]
use crate::spine::SpineRootCompactResult;
#[cfg(test)]
use crate::spine::SpineToolOutputRecording;
use crate::spine::SpineTrimOutcome;
use crate::spine::TrimBodyUpdate;
use crate::spine::TrimResponseKind;
use crate::spine::adapter::projection::build_spine_tree_context_annotations;
use crate::spine::adapter::projection::build_spine_tree_inside_view_from_projection;
use crate::spine::bridge::CompletedToolCallHostOutcome;
use crate::spine::bridge::ReplayRootCompactBoundary;
use crate::spine::bridge::ReplayRuntime;
use crate::spine::bridge::ToolcallPreparedHostCommit;
use crate::spine::bridge::TreeSnapshotProjection;
use crate::spine::bridge::TrimRequest;
use crate::spine::bridge::is_non_toolcall_msg;
use crate::spine::bridge::prepare_completed_toolcall_for_commit;
use crate::spine::bridge::prepare_grouped_output_recording;
use crate::spine::bridge::prepare_single_output_recording;
use crate::spine::conflicting_spine_control_rejection_reason;
use crate::spine::hooks;
use crate::spine::hooks::CompactEvidence;
use crate::spine::hooks::HostEffects;
use crate::spine::hooks::InitEvidence;
use crate::spine::hooks::MessageEvidence;
use crate::spine::hooks::ToolCallEvidence;
use crate::spine::is_spine_parser_control_tool;
use crate::spine::spine_tool_use_failed_message;
use crate::stream_events_utils::InFlightFuture;
use crate::stream_events_utils::is_spine_control_function_call;
use crate::tools::ToolRouter;
use crate::tools::context::ToolPayload;
use crate::tools::router::ToolCall;
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

#[derive(Clone)]
struct RecordedToolOutput {
    call_id: String,
    raw_ordinal: u64,
    context_index: usize,
    item: ResponseItem,
}

impl PreparedSpineReplay {
    pub(super) fn new(replay: ReplayRuntime) -> Self {
        Self { replay }
    }
}

impl Session {
    pub(super) async fn restore_context_from_rollout(
        &self,
        turn_context: &TurnContext,
        rollout_items: &[RolloutItem],
    ) -> CodexResult<Option<PreviousTurnSettings>> {
        let reconstructed_rollout = self
            .reconstruct_history_from_rollout(turn_context, rollout_items)
            .await;
        self.apply_spine_rollout_reconstruction(reconstructed_rollout)
            .await
            .map_err(|err| {
                CodexErr::Fatal(format!(
                    "failed to rebuild Spine runtime from rollout: {err}"
                ))
            })
    }

    pub(super) fn merge_fixed_context_with_spine_history(
        reconstructed_history: Vec<ResponseItem>,
        spine_history: Vec<ResponseItem>,
    ) -> Vec<ResponseItem> {
        let Some(first_variable) = reconstructed_history
            .iter()
            .position(|item| !crate::spine::bridge::is_spine_fixed_prefix_item(item))
        else {
            let mut history = reconstructed_history;
            history.extend(spine_history);
            return history;
        };
        let last_variable = reconstructed_history
            .iter()
            .rposition(|item| !crate::spine::bridge::is_spine_fixed_prefix_item(item))
            .expect("first variable item exists");

        let mut history = Vec::with_capacity(
            first_variable
                + spine_history.len()
                + reconstructed_history
                    .len()
                    .saturating_sub(last_variable + 1),
        );
        history.extend(reconstructed_history[..first_variable].iter().cloned());
        history.extend(spine_history);
        history.extend(reconstructed_history[last_variable + 1..].iter().cloned());
        history
    }

    pub(super) async fn apply_spine_rollout_reconstruction(
        &self,
        reconstructed_rollout: RolloutReconstruction,
    ) -> CodexResult<Option<PreviousTurnSettings>> {
        let previous_turn_settings = reconstructed_rollout.previous_turn_settings.clone();
        let spine_history = self
            .apply_spine_rollout_replay(&reconstructed_rollout)
            .await?;
        let history = if let Some(spine_history) = spine_history {
            Self::merge_fixed_context_with_spine_history(
                reconstructed_rollout.history,
                spine_history,
            )
        } else {
            reconstructed_rollout.history
        };
        self.replace_history(history, reconstructed_rollout.reference_context_item)
            .await;
        self.set_previous_turn_settings(previous_turn_settings.clone())
            .await;
        Ok(previous_turn_settings)
    }

    async fn apply_spine_rollout_replay(
        &self,
        reconstructed_rollout: &RolloutReconstruction,
    ) -> CodexResult<Option<Vec<ResponseItem>>> {
        let replay_raw_len = u64::try_from(reconstructed_rollout.raw_response_items.len())
            .map_err(|_| CodexErr::Fatal("raw response item count overflow".to_string()))?;
        let spine_replay = if self.features.enabled(Feature::SpineJit) {
            self.prepare_spine_replay_from_rollout_items(
                &reconstructed_rollout.raw_response_items,
                &reconstructed_rollout.spine_rollback_cuts,
                reconstructed_rollout.used_replacement_history,
                reconstructed_rollout
                    .base_replacement_history_boundary
                    .as_ref(),
                &reconstructed_rollout.replacement_history_boundaries,
            )
            .await
            .map_err(|err| {
                CodexErr::Fatal(format!(
                    "failed to rebuild Spine runtime from rollout: {err}"
                ))
            })?
        } else if self.features.enabled(Feature::SpineTrim) {
            self.prepare_spine_trim_replay_from_rollout_items(
                replay_raw_len,
                &reconstructed_rollout.history,
            )
            .await
            .map_err(|err| {
                CodexErr::Fatal(format!(
                    "failed to rebuild Spine trim runtime from rollout: {err}"
                ))
            })?
        } else {
            None
        };
        if let Some(spine_replay) = spine_replay {
            self.apply_spine_replay(spine_replay).await.map_err(|err| {
                CodexErr::Fatal(format!(
                    "failed to rebuild Spine runtime from rollout: {err}"
                ))
            })
        } else {
            Ok(None)
        }
    }
}

#[derive(Debug)]
#[cfg(test)]
pub(crate) struct SpineToolCommit {
    recording: SpineToolOutputRecording,
    deferred_tree_update: Option<SpineTreeUpdateEvent>,
}

pub(crate) enum SpineToolcallTurnError {
    Terminal(String),
}

pub(crate) struct DeferredSpineToolCall {
    pub(crate) call: ToolCall,
    pub(crate) in_flight: Option<InFlightFuture<'static>>,
}

pub(crate) enum DeferredSpineToolGroup {
    Normal(Vec<DeferredSpineToolCall>),
    ConflictingControls {
        group: Vec<DeferredSpineToolCall>,
        message: String,
    },
}

pub(crate) struct DeferredSpineToolGroupCommit {
    pub(crate) commit_call_id: String,
    pub(crate) tool_call_ids: Vec<String>,
}

pub(crate) struct DeferredSpineToolRequestPlan {
    pub(crate) records_control_overlay: bool,
    pub(crate) starts_native_tool: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InFlightSpineToolOutputPlan {
    RecordSpineToolOutput,
    RecordControlOverlayOnly,
    RecordOrdinaryToolOutput { apply_trim_projection: bool },
}

pub(crate) struct DeferredSpineConflictingControlCommit {
    commit_call_id: String,
    tool_call_ids: Vec<String>,
    control_call_ids: Vec<String>,
    response_slots: Vec<Option<ResponseItem>>,
}

impl std::fmt::Display for SpineToolcallTurnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Terminal(message) => f.write_str(message),
        }
    }
}

impl DeferredSpineConflictingControlCommit {
    pub(crate) fn has_prepared_response_slot(&self, index: usize) -> bool {
        self.response_slots.get(index).is_some_and(Option::is_some)
    }

    pub(crate) fn fill_response_slot(
        &mut self,
        index: usize,
        response_item: ResponseItem,
    ) -> Result<(), SpineToolcallTurnError> {
        let slot = self.response_slots.get_mut(index).ok_or_else(|| {
            SpineToolcallTurnError::Terminal(
                "conflicting Spine tool request output index outside group".into(),
            )
        })?;
        *slot = Some(response_item);
        Ok(())
    }

    pub(crate) fn into_parts(
        self,
    ) -> Result<(String, Vec<String>, Vec<String>, Vec<ResponseItem>), SpineToolcallTurnError> {
        let mut response_items = Vec::with_capacity(self.response_slots.len());
        for (index, item) in self.response_slots.into_iter().enumerate() {
            let item = item.ok_or_else(|| {
                SpineToolcallTurnError::Terminal(format!(
                    "conflicting Spine tool request missing output for call_id={}",
                    self.tool_call_ids
                        .get(index)
                        .map_or("<unknown>", String::as_str)
                ))
            })?;
            response_items.push(item);
        }
        Ok((
            self.commit_call_id,
            self.tool_call_ids,
            self.control_call_ids,
            response_items,
        ))
    }
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

#[cfg(test)]
fn tool_commit_from_host_outcome(outcome: CompletedToolCallHostOutcome) -> SpineToolCommit {
    let (recording, deferred_tree_update) = outcome.into_test_parts();
    SpineToolCommit {
        recording,
        deferred_tree_update,
    }
}

#[derive(Debug, Default)]
pub(crate) struct SpineControlOverlay {
    enabled: bool,
    items: Vec<ResponseItem>,
}

impl SpineControlOverlay {
    pub(crate) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            items: Vec::new(),
        }
    }

    pub(crate) fn push_request(&mut self, item: ResponseItem) {
        // FormularDef 3.1.4.5: this is a turn-local protocol closure overlay for
        // Spine tool request/output pairs. It is not ContextManager history,
        // sidecar state, or h(PS), and feature-off must be base Codex.
        if !self.enabled {
            return;
        }
        self.items.push(item);
    }

    pub(crate) fn push_output_if_matching(&mut self, item: &ResponseItem) {
        if !self.enabled {
            return;
        }
        if let ResponseItem::FunctionCallOutput { call_id, .. } = item
            && self.contains_call_id(call_id)
        {
            self.items.push(item.clone());
        }
    }

    pub(crate) fn contains_matching_request(&self, item: &ResponseItem) -> bool {
        if !self.enabled {
            return false;
        }
        match item {
            ResponseItem::FunctionCallOutput { call_id, .. } => self.contains_call_id(call_id),
            _ => false,
        }
    }

    fn contains_call_id(&self, call_id: &str) -> bool {
        self.items.iter().any(|item| {
            matches!(
                item,
                ResponseItem::FunctionCall {
                    call_id: existing,
                    ..
                } if existing == call_id
            )
        })
    }

    pub(crate) fn remove_call_ids(&mut self, call_ids: &[String]) {
        if !self.enabled {
            return;
        }
        self.items.retain(|item| {
            let item_call_id = match item {
                ResponseItem::FunctionCall { call_id, .. }
                | ResponseItem::FunctionCallOutput { call_id, .. } => Some(call_id.as_str()),
                _ => None,
            };
            !item_call_id.is_some_and(|call_id| call_ids.iter().any(|existing| existing == call_id))
        });
    }

    pub(crate) fn take_for_next_prompt(&mut self) -> Vec<ResponseItem> {
        if !self.enabled {
            return Vec::new();
        }
        std::mem::take(&mut self.items)
    }
}

impl Session {
    pub(crate) fn new_spine_control_overlay(&self) -> SpineControlOverlay {
        SpineControlOverlay::new(self.features.enabled(Feature::SpineJit))
    }

    #[cfg(test)]
    pub(crate) fn spine_control_overlay_for_enabled(enabled: bool) -> SpineControlOverlay {
        SpineControlOverlay::new(enabled)
    }

    pub(crate) fn spine_control_overlay_request_item(item: &ResponseItem) -> Option<ResponseItem> {
        is_spine_control_function_call(item).then(|| item.clone())
    }

    pub(crate) fn deferred_spine_tool_call(
        &self,
        item: ResponseItem,
    ) -> Result<Option<ToolCall>, FunctionCallError> {
        Self::deferred_spine_tool_call_for_enabled(self.features.enabled(Feature::SpineJit), item)
    }

    pub(crate) fn deferred_spine_tool_call_for_enabled(
        enabled: bool,
        item: ResponseItem,
    ) -> Result<Option<ToolCall>, FunctionCallError> {
        if !enabled {
            return Ok(None);
        }
        ToolRouter::build_tool_call(item)
    }

    pub(crate) fn should_drain_pending_deferred_spine_tool_calls(
        &self,
        deferred_tool_calls: &[DeferredSpineToolCall],
        has_new_deferred_tool_call: bool,
    ) -> bool {
        Self::should_drain_pending_deferred_spine_tool_calls_for_enabled(
            self.features.enabled(Feature::SpineJit),
            deferred_tool_calls,
            has_new_deferred_tool_call,
        )
    }

    pub(crate) fn should_drain_pending_deferred_spine_tool_calls_for_enabled(
        enabled: bool,
        deferred_tool_calls: &[DeferredSpineToolCall],
        has_new_deferred_tool_call: bool,
    ) -> bool {
        enabled && !has_new_deferred_tool_call && !deferred_tool_calls.is_empty()
    }

    pub(crate) fn in_flight_spine_tool_output_plan(
        &self,
        matches_control_overlay: bool,
    ) -> InFlightSpineToolOutputPlan {
        Self::in_flight_spine_tool_output_plan_for_enabled(
            self.features.enabled(Feature::SpineJit),
            self.features.enabled(Feature::SpineTrim),
            matches_control_overlay,
        )
    }

    pub(crate) fn in_flight_spine_tool_output_plan_for_enabled(
        spine_jit_enabled: bool,
        spine_trim_enabled: bool,
        matches_control_overlay: bool,
    ) -> InFlightSpineToolOutputPlan {
        if spine_jit_enabled {
            return InFlightSpineToolOutputPlan::RecordSpineToolOutput;
        }
        if matches_control_overlay {
            return InFlightSpineToolOutputPlan::RecordControlOverlayOnly;
        }
        InFlightSpineToolOutputPlan::RecordOrdinaryToolOutput {
            apply_trim_projection: spine_trim_enabled,
        }
    }

    pub(crate) fn is_spine_parser_control_tool_call(call: &ToolCall) -> bool {
        is_spine_parser_control_tool(
            call.tool_name.namespace.as_deref(),
            call.tool_name.name.as_str(),
        )
    }

    pub(crate) fn conflicting_spine_control_rejection_output(
        call: &ToolCall,
        message: &str,
    ) -> ResponseItem {
        let output = FunctionCallOutputPayload {
            body: FunctionCallOutputBody::Text(spine_tool_use_failed_message(message)),
            success: Some(false),
        };
        match &call.payload {
            ToolPayload::Custom { .. } => ResponseItem::CustomToolCallOutput {
                call_id: call.call_id.clone(),
                name: None,
                output,
            },
            ToolPayload::ToolSearch { .. } => ResponseItem::ToolSearchOutput {
                call_id: Some(call.call_id.clone()),
                status: "completed".to_string(),
                execution: "client".to_string(),
                tools: Vec::new(),
            },
            ToolPayload::Function { .. } => ResponseItem::FunctionCallOutput {
                call_id: call.call_id.clone(),
                output,
            },
        }
    }

    pub(crate) fn conflicting_spine_control_rejection_reason_for_calls(
        calls: &[&ToolCall],
    ) -> String {
        let names = calls
            .iter()
            .filter(|call| Self::is_spine_parser_control_tool_call(call))
            .map(|call| format!("{} ({})", call.tool_name.name, call.call_id))
            .collect::<Vec<_>>()
            .join(", ");
        conflicting_spine_control_rejection_reason(&names)
    }

    pub(crate) fn take_deferred_spine_tool_group(
        deferred_tool_calls: &mut Vec<DeferredSpineToolCall>,
    ) -> Option<DeferredSpineToolGroup> {
        if deferred_tool_calls.is_empty() {
            return None;
        }
        let spine_control_count = deferred_tool_calls
            .iter()
            .filter(|deferred| Self::is_spine_parser_control_tool_call(&deferred.call))
            .count();
        match spine_control_count {
            0 | 1 => Some(DeferredSpineToolGroup::Normal(std::mem::take(
                deferred_tool_calls,
            ))),
            _ => {
                let control_calls = deferred_tool_calls
                    .iter()
                    .filter(|deferred| Self::is_spine_parser_control_tool_call(&deferred.call))
                    .map(|deferred| &deferred.call)
                    .collect::<Vec<_>>();
                let message = Self::conflicting_spine_control_rejection_reason_for_calls(
                    control_calls.as_slice(),
                );
                Some(DeferredSpineToolGroup::ConflictingControls {
                    group: std::mem::take(deferred_tool_calls),
                    message,
                })
            }
        }
    }

    pub(crate) fn deferred_spine_tool_request_plan(
        call: &ToolCall,
    ) -> DeferredSpineToolRequestPlan {
        let is_control = Self::is_spine_parser_control_tool_call(call);
        DeferredSpineToolRequestPlan {
            records_control_overlay: is_control,
            starts_native_tool: !is_control,
        }
    }

    pub(crate) fn deferred_spine_tool_group_commit(
        group: &[DeferredSpineToolCall],
    ) -> Result<DeferredSpineToolGroupCommit, SpineToolcallTurnError> {
        let commit_call_id = group
            .iter()
            .find(|deferred| Self::is_spine_parser_control_tool_call(&deferred.call))
            .map(|deferred| deferred.call.call_id.clone())
            .or_else(|| group.first().map(|deferred| deferred.call.call_id.clone()))
            .ok_or_else(|| {
                SpineToolcallTurnError::Terminal("grouped Spine toolcall missing tool call".into())
            })?;
        let tool_call_ids = group
            .iter()
            .map(|deferred| deferred.call.call_id.clone())
            .collect::<Vec<_>>();
        Ok(DeferredSpineToolGroupCommit {
            commit_call_id,
            tool_call_ids,
        })
    }

    pub(crate) fn deferred_spine_conflicting_control_commit(
        group: &[DeferredSpineToolCall],
        message: &str,
    ) -> Result<DeferredSpineConflictingControlCommit, SpineToolcallTurnError> {
        let commit_call_id = group
            .iter()
            .find(|deferred| Self::is_spine_parser_control_tool_call(&deferred.call))
            .map(|deferred| deferred.call.call_id.clone())
            .ok_or_else(|| {
                SpineToolcallTurnError::Terminal(
                    "conflicting Spine tool request missing parser-control call".into(),
                )
            })?;
        let tool_call_ids = group
            .iter()
            .map(|deferred| deferred.call.call_id.clone())
            .collect::<Vec<_>>();
        let mut control_call_ids = Vec::new();
        let mut response_slots = std::iter::repeat_with(|| None)
            .take(group.len())
            .collect::<Vec<_>>();
        for (index, deferred) in group.iter().enumerate() {
            if Self::is_spine_parser_control_tool_call(&deferred.call) {
                control_call_ids.push(deferred.call.call_id.clone());
                response_slots[index] = Some(Self::conflicting_spine_control_rejection_output(
                    &deferred.call,
                    message,
                ));
            }
        }
        Ok(DeferredSpineConflictingControlCommit {
            commit_call_id,
            tool_call_ids,
            control_call_ids,
            response_slots,
        })
    }

    pub(crate) async fn send_spine_tree_update(
        &self,
        turn_context: &TurnContext,
        snapshot: SpineTreeUpdateEvent,
    ) {
        self.send_event(turn_context, EventMsg::SpineTreeUpdate(snapshot))
            .await;
    }

    pub(crate) async fn on_toolcall(
        &self,
        turn_context: &TurnContext,
        evidence: ToolCallEvidence<'_>,
    ) -> Result<(), SpineToolcallTurnError> {
        self.commit_toolcall_evidence(turn_context, evidence)
            .await
            .map_err(|err| SpineToolcallTurnError::Terminal(err.to_string()))
    }

    pub(crate) async fn record_single_spine_tool_output(
        &self,
        turn_context: &TurnContext,
        response_item: &ResponseItem,
    ) -> Result<(), SpineToolcallTurnError> {
        self.on_toolcall(turn_context, ToolCallEvidence::single(response_item))
            .await
    }

    pub(crate) async fn record_grouped_spine_tool_output(
        &self,
        turn_context: &TurnContext,
        commit_call_id: &str,
        tool_call_ids: &[String],
        response_items: &[ResponseItem],
    ) -> Result<(), SpineToolcallTurnError> {
        self.on_toolcall(
            turn_context,
            ToolCallEvidence::grouped(commit_call_id, tool_call_ids, response_items),
        )
        .await
    }

    pub(crate) async fn record_grouped_ordinary_tool_output(
        &self,
        turn_context: &TurnContext,
        commit_call_id: &str,
        tool_call_ids: &[String],
        response_items: &[ResponseItem],
    ) -> Result<(), SpineToolcallTurnError> {
        self.on_toolcall(
            turn_context,
            ToolCallEvidence::grouped_as_ordinary(commit_call_id, tool_call_ids, response_items),
        )
        .await
    }

    async fn commit_toolcall_evidence(
        &self,
        turn_context: &TurnContext,
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
        self.apply_completed_spine_toolcall_host_outcome(turn_context, &mut outcome)
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
        let post_commit_effects = outcome.take_post_commit_effects();
        let deferred_tree_update = self
            .apply_spine_post_commit_effects(turn_context, post_commit_effects)
            .await;
        outcome.set_deferred_tree_update(deferred_tree_update);
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

    pub(crate) async fn apply_trim_projection_if_available(&self) -> Result<(), SpineError> {
        self.apply_spine_trim_projection_if_available().await
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
        let mut guard = spine_slot.lock().await;
        guard.install_cloned_sidecar_for_fork(boundary, &target_rollout_path, raw_items)
    }

    pub(super) async fn clone_spine_sidecar_for_fork_if_needed(
        &self,
        spine_fork_source_boundary: Option<&SpineCloneBoundary>,
        initial_history: &InitialHistory,
    ) -> Result<(), SpineError> {
        let Some(boundary) = spine_fork_source_boundary else {
            return Ok(());
        };
        if !(self.features.enabled(Feature::SpineJit) || self.features.enabled(Feature::SpineTrim))
        {
            return Ok(());
        }
        if !matches!(initial_history, InitialHistory::Forked(_)) {
            return Ok(());
        }
        let raw_items = spine_raw_items_after_rollback(&initial_history.get_rollout_items());
        self.clone_spine_sidecar_for_fork(boundary, &raw_items)
            .await
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
                .map(|(raw_boundary, variable_replacement_history)| {
                    ReplayRootCompactBoundary::new(*raw_boundary, variable_replacement_history)
                })
                .collect::<Vec<_>>();
            prepared_runtime.validate_rollout_compact_boundaries(
                &rollout_path,
                &raw_live,
                raw_items,
                ReplayRootCompactBoundary::new(
                    base_boundary.raw_boundary,
                    &base_variable_replacement_history,
                ),
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
            .filter(|item| !crate::spine::bridge::is_spine_fixed_prefix_item(item))
            .cloned()
            .collect()
    }

    pub(super) async fn observe_spine_raw_items(&self, count: usize) -> Result<(), SpineError> {
        let Some(spine_slot) = self.spine.as_ref() else {
            return Ok(());
        };
        let mut guard = spine_slot.lock().await;
        guard.observe_raw_items(count)
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
        guard.ensure_runtime(&rollout_path)
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

    pub(crate) async fn abort_pending_turn_commit_after_turn_abort(&self) -> Option<String> {
        self.abort_stale_spine_pending("turn aborted before pending Spine commit")
            .await
    }

    async fn abort_stale_spine_pending(&self, reason: &str) -> Option<String> {
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

    pub(crate) async fn close_pending_turn_commit_as_aborted_toolcall(
        self: &Arc<Self>,
        turn_context: &Arc<TurnContext>,
    ) -> Result<Option<String>, SpineToolcallTurnError> {
        self.close_stale_spine_pending_as_aborted_toolcall(
            turn_context,
            "turn aborted before pending Spine toolcall completed",
        )
        .await
    }

    async fn close_stale_spine_pending_as_aborted_toolcall(
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

    pub(crate) async fn observe_provider_input_tokens_for_projection(
        &self,
        input_tokens: Option<i64>,
    ) {
        let Some(spine_slot) = self.spine.as_ref() else {
            return;
        };
        let mut guard = spine_slot.lock().await;
        guard.observe_provider_token_usage(input_tokens);
    }

    pub(super) async fn observe_spine_context_items(
        &self,
        turn_context: &TurnContext,
        raw_ordinals: &[Option<u64>],
        items: &[ResponseItem],
        appends: &[ContextAppend],
    ) -> Result<(), SpineError> {
        let Some(spine_slot) = self.spine.as_ref() else {
            return Ok(());
        };
        {
            let guard = spine_slot.lock().await;
            guard.ensure_observable_context()?;
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
        let mut recorded_tool_outputs = Vec::<RecordedToolOutput>::new();
        let mut trim_tool_outputs = Vec::<RecordedToolOutput>::new();
        let mut observed_tool_request_call_ids = Vec::<String>::new();
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
                self.flush_recorded_tool_outputs_as_toolcall(
                    turn_context,
                    &mut recorded_tool_outputs,
                )
                .await?;
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
                    guard.observe_context_item(raw_ordinal, context_index, item)?;
                }
                if let Some(call_id) = tool_request_call_id_for_completed_toolcall(item)
                    && !observed_tool_request_call_ids
                        .iter()
                        .any(|existing| existing == call_id)
                {
                    observed_tool_request_call_ids.push(call_id.to_string());
                }
                if let Some(call_id) = tool_response_call_id_for_trim(item) {
                    let is_spine_control_output =
                        self.is_spine_control_output_response_item(item).await?;
                    if !is_spine_control_output {
                        let output = RecordedToolOutput {
                            call_id: call_id.to_string(),
                            raw_ordinal,
                            context_index,
                            item: item.clone(),
                        };
                        trim_tool_outputs.push(output.clone());
                        if has_completed_toolcall_request_anchor(
                            call_id,
                            &observed_tool_request_call_ids,
                            history_items,
                            &raw_items,
                        ) {
                            recorded_tool_outputs.push(output);
                        }
                    }
                }
            }
        }
        self.observe_recorded_tool_outputs_for_trim(&trim_tool_outputs, &raw_items)
            .await?;
        self.flush_recorded_tool_outputs_as_toolcall(turn_context, &mut recorded_tool_outputs)
            .await?;
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

    async fn observe_recorded_tool_outputs_for_trim(
        &self,
        recorded_tool_outputs: &[RecordedToolOutput],
        raw_items: &[Option<ResponseItem>],
    ) -> Result<(), SpineError> {
        if recorded_tool_outputs.is_empty() {
            return Ok(());
        }
        let Some(spine_slot) = self.spine.as_ref() else {
            return Ok(());
        };
        let tool_responses = recorded_tool_outputs
            .iter()
            .map(|output| {
                (
                    output.call_id.clone(),
                    output.raw_ordinal,
                    output.context_index,
                )
            })
            .collect::<Vec<_>>();
        let mut guard = spine_slot.lock().await;
        guard.observe_recorded_tool_outputs_for_trim(&tool_responses, raw_items)?;
        Ok(())
    }

    async fn flush_recorded_tool_outputs_as_toolcall(
        &self,
        turn_context: &TurnContext,
        recorded_tool_outputs: &mut Vec<RecordedToolOutput>,
    ) -> Result<(), SpineError> {
        if recorded_tool_outputs.is_empty() {
            return Ok(());
        }
        let commit_call_id = recorded_tool_outputs[0].call_id.clone();
        let mut tool_call_ids = Vec::<String>::new();
        for output in recorded_tool_outputs.iter() {
            if !tool_call_ids.contains(&output.call_id) {
                tool_call_ids.push(output.call_id.clone());
            }
        }
        let output_items = recorded_tool_outputs
            .iter()
            .map(|output| output.item.clone())
            .collect::<Vec<_>>();
        let output_raw_ordinals = recorded_tool_outputs
            .iter()
            .map(|output| Some(output.raw_ordinal))
            .collect::<Vec<_>>();
        let output_context_indices = recorded_tool_outputs
            .iter()
            .map(|output| output.context_index)
            .collect::<Vec<_>>();
        let commit = self.on_toolcall(
            turn_context,
            ToolCallEvidence::grouped_already_recorded(
                &commit_call_id,
                &tool_call_ids,
                &output_items,
                &output_raw_ordinals,
                &output_context_indices,
            ),
        );
        Box::pin(commit)
            .await
            .map_err(|err| SpineError::Operation(err.to_string()))?;
        recorded_tool_outputs.clear();
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
        guard.ensure_runtime(&rollout_path)?;
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
        guard.test_seed_open_control_request(call_id, summary, &raw_items)
    }

    #[cfg(test)]
    pub(crate) async fn test_seed_spine_close_control_request<M: IntoSpineNodeMemory>(
        &self,
        call_id: String,
        memory: M,
    ) -> Result<(), SpineError> {
        let raw_items = self.spine_raw_items_from_rollout().await?;
        let spine = self.ensure_spine_runtime().await?;
        let mut guard = spine.lock().await;
        guard.test_seed_close_control_request(call_id, memory, &raw_items)
    }

    #[cfg(test)]
    pub(crate) async fn test_seed_spine_next_control_request<M: IntoSpineNodeMemory>(
        &self,
        call_id: String,
        summary: String,
        memory: M,
    ) -> Result<(), SpineError> {
        let raw_items = self.spine_raw_items_from_rollout().await?;
        let spine = self.ensure_spine_runtime().await?;
        let mut guard = spine.lock().await;
        guard.test_seed_next_control_request(call_id, summary, memory, &raw_items)
    }

    pub(crate) async fn trim_spine_tool_response(
        &self,
        trim_id: String,
    ) -> Result<SpineTrimOutcome, SpineError> {
        self.apply_spine_trim_request(trim_id, TrimRequest::Snip)
            .await
    }

    pub(crate) async fn slice_spine_tool_response_head(
        &self,
        trim_id: String,
        head: usize,
    ) -> Result<SpineTrimOutcome, SpineError> {
        self.apply_spine_trim_request(trim_id, TrimRequest::SliceHead { head })
            .await
    }

    pub(crate) async fn slice_spine_tool_response_tail(
        &self,
        trim_id: String,
        tail: usize,
    ) -> Result<SpineTrimOutcome, SpineError> {
        self.apply_spine_trim_request(trim_id, TrimRequest::SliceTail { tail })
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
    ) -> Result<SpineTrimOutcome, SpineError> {
        let raw_items = if request.needs_raw_items() {
            Some(self.spine_raw_items_from_rollout().await?)
        } else {
            None
        };
        let spine = self.ensure_spine_runtime().await?;
        let (outcome, updates) = {
            let mut guard = spine.lock().await;
            request
                .apply_to_state(&mut guard, &trim_id, raw_items.as_deref())?
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
        &self,
        turn_context: &TurnContext,
        spine_slot: &Mutex<SpineSessionState>,
        evidence: ToolCallEvidence<'a>,
    ) -> Result<Option<ToolcallPreparedHostCommit<'a>>, SpineError> {
        prepare_completed_toolcall_for_commit(
            &evidence,
            || async { self.clone_history().await },
            || async { self.spine_raw_items_from_rollout_for_commit().await },
            |call_id, raw_items| async move {
                let guard = spine_slot.lock().await;
                prepare_single_output_recording(&guard, &call_id, &raw_items)
            },
            |output_items| async move {
                let guard = spine_slot.lock().await;
                prepare_grouped_output_recording(&guard, &output_items)
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
        &self,
        turn_context: &TurnContext,
        toolcall: ToolcallPreparedHostCommit<'_>,
    ) -> Result<CompletedToolCallHostOutcome, SpineError> {
        let Some(spine_slot) = self.spine.as_ref() else {
            return Ok(CompletedToolCallHostOutcome::no_spine_commit());
        };
        let call_id = toolcall.call_id();
        let item = toolcall.response_item();
        let tool_resp_already_recorded = toolcall.response_already_recorded();
        let raw_items = self.spine_raw_items_from_rollout_for_commit().await?;
        let current_turn_token_info = self.current_turn_token_usage_info(turn_context).await;
        let current_turn_provider_input_tokens = current_turn_token_info
            .as_ref()
            .and_then(provider_input_context_tokens);
        let toolcall_host_effects = {
            let mut guard = spine_slot.lock().await;
            toolcall.prepare_host_effects(
                &mut guard,
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
                            let Ok(mut guard) = spine_slot.try_lock() else {
                                return Ok(
                                    crate::spine::bridge::ToolcallHostAttempt::host_lock_busy(),
                                );
                            };
                            let Ok(mut state) = self.state.try_lock() else {
                                return Ok(
                                    crate::spine::bridge::ToolcallHostAttempt::host_lock_busy(),
                                );
                            };
                            let reference_context_item = state.reference_context_item();
                            let history = state.clone_history();
                            let token_info = state.token_info();
                            attempt.attempt_with_host_state(
                                item,
                                tool_resp_already_recorded,
                                raw_items,
                                &mut guard,
                                history.raw_items(),
                                reference_context_item,
                                expected_history,
                                |host_effects| {
                                    Self::apply_spine_host_effects_to_locked_state(
                                        &mut state,
                                        host_effects,
                                    )
                                },
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
                    },
                    || async {
                        tokio::task::yield_now().await;
                    },
                    |reason| {
                        let call_id = call_id.to_string();
                        async move {
                            self.fail_closed_spine_toolcall_commit(&call_id, reason)
                                .await;
                        }
                    },
                    |reason| {
                        let call_id = call_id.to_string();
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
    ) -> Result<Option<(SpineRootCompactResult, SpineTreeUpdateEvent)>, SpineError> {
        let Some(spine_slot) = self.spine.as_ref() else {
            return Ok(None);
        };
        let Some(prepared) = self.prepare_spine_root_compact_impl(body).await? else {
            return Ok(None);
        };
        let publication = prepared.variable_context_publication_for_test();
        let mut guard = spine_slot.lock().await;
        let snapshot = guard.apply_root_compact_after_history_publish(
            prepared,
            publication.variable_context().len(),
        )?;
        Ok(Some((publication, snapshot)))
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
            .apply_history_publication(
                self.spine.as_ref(),
                items,
                crate::spine::bridge::is_spine_fixed_prefix_item,
                |reason| CodexErr::SpineTerminalFailure {
                    operation: "install Spine root compact".to_string(),
                    reason,
                },
                compacted_item,
                |published_items, compacted_item| {
                    let reference_context_item = publish_reference_context_item;
                    async move {
                        self.publish_spine_root_compact_history(
                            published_items,
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
        published_items: Vec<ResponseItem>,
        reference_context_item: Option<TurnContextItem>,
        compacted_item: CompactedItem,
    ) -> CodexResult<()> {
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

fn tool_request_call_id_for_completed_toolcall(item: &ResponseItem) -> Option<&str> {
    match item {
        ResponseItem::FunctionCall { call_id, .. }
        | ResponseItem::CustomToolCall { call_id, .. } => Some(call_id.as_str()),
        _ => None,
    }
}

fn tool_response_call_id_for_trim(item: &ResponseItem) -> Option<&str> {
    match item {
        ResponseItem::FunctionCallOutput { call_id, .. }
        | ResponseItem::CustomToolCallOutput { call_id, .. } => Some(call_id.as_str()),
        _ => None,
    }
}

fn has_completed_toolcall_request_anchor(
    call_id: &str,
    observed_tool_request_call_ids: &[String],
    history_items: &[ResponseItem],
    raw_items: &[Option<ResponseItem>],
) -> bool {
    observed_tool_request_call_ids
        .iter()
        .any(|existing| existing == call_id)
        || history_items
            .iter()
            .any(|item| tool_request_call_id_for_completed_toolcall(item) == Some(call_id))
        || raw_items.iter().any(|item| {
            item.as_ref()
                .and_then(tool_request_call_id_for_completed_toolcall)
                == Some(call_id)
        })
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
    pub(super) async fn prepare_spine_append_observation(
        &self,
        items: &[ResponseItem],
    ) -> CodexResult<SpineAppendObservation> {
        let Some(spine_slot) = self.spine.as_ref() else {
            return Ok(SpineAppendObservation::disabled());
        };
        let raw_start = spine_slot.lock().await.raw_len();
        match SpineAppendObservation::new(raw_start, items) {
            Ok(observation) => Ok(observation),
            Err(err) => Err(self
                .spine_append_fatal("assign Spine raw ordinals", err)
                .await),
        }
    }

    pub(super) async fn observe_spine_raw_items_for_append(
        &self,
        observation: &SpineAppendObservation,
    ) -> CodexResult<()> {
        if observation.is_disabled() {
            return Ok(());
        }
        if let Err(err) = self.ensure_spine_runtime_if_available().await {
            return Err(self
                .spine_append_fatal("initialize Spine runtime", err)
                .await);
        }
        if let Err(err) = self
            .observe_spine_raw_items(observation.persisted_raw_count())
            .await
        {
            return Err(self
                .spine_append_fatal("observe Spine raw items", err)
                .await);
        }
        Ok(())
    }

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

pub(super) struct SpineAppendObservation {
    enabled: bool,
    raw_ordinals: Vec<Option<u64>>,
    persisted_raw_count: usize,
}

impl SpineAppendObservation {
    fn disabled() -> Self {
        Self {
            enabled: false,
            raw_ordinals: Vec::new(),
            persisted_raw_count: 0,
        }
    }

    fn new(raw_start: u64, items: &[ResponseItem]) -> Result<Self, SpineError> {
        let (raw_ordinals, persisted_raw_count) = assign_spine_raw_ordinals(raw_start, items)?;
        Ok(Self {
            enabled: true,
            raw_ordinals,
            persisted_raw_count,
        })
    }

    pub(super) fn raw_ordinals(&self) -> &[Option<u64>] {
        &self.raw_ordinals
    }

    pub(super) fn has_raw_ordinals(&self) -> bool {
        !self.raw_ordinals.is_empty()
    }

    fn persisted_raw_count(&self) -> usize {
        self.persisted_raw_count
    }

    fn is_disabled(&self) -> bool {
        !self.enabled
    }
}

fn assign_spine_raw_ordinals(
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
