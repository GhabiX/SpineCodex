use super::*;
use crate::client::ModelClientSession;
use crate::context_manager::ContextAppend;
use crate::context_manager::ContextManager;
use crate::context_manager::estimate_response_item_model_visible_bytes;
use crate::session::rollout_reconstruction::ReplacementHistoryBoundary;
use crate::session::spine_compact::SpineCloseCompactOutcome;
use crate::session::spine_tree_inside::build_spine_tree_inside_view;
#[cfg(test)]
use crate::session::turn::built_tools;
use crate::spine::CompletedToolCall;
use crate::spine::CompletedToolCallSegment;
use crate::spine::LiveRootCompact;
use crate::spine::NodeId;
use crate::spine::SpineCloneBoundary;
use crate::spine::SpineCloseCompact;
use crate::spine::SpineCommitKind;
use crate::spine::SpineCompactSourcePlan;
use crate::spine::SpinePendingCloseAction;
use crate::spine::SpinePendingCommit;
use crate::spine::SpineRootCompactResult;
use crate::spine::SpineRootCompactTokenMetadata;
use crate::spine::SpineRuntime;
use crate::spine::SpineStore;
use crate::spine::SpineTokenBaselines;
use crate::spine::ToolCallSegmentKind;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::TokenUsageInfo;
use codex_protocol::protocol::TurnContextItem;
use codex_protocol::spine_tree::SpineTreeUpdateEvent;
use codex_rollout::should_persist_response_item;
use codex_tools::ToolSpec;
use codex_utils_output_truncation::approx_tokens_from_byte_count_i64;
#[cfg(test)]
use std::collections::HashSet;
#[cfg(test)]
use tokio_util::sync::CancellationToken;

pub(super) struct PreparedSpineReplay {
    raw_len: u64,
    runtime: Option<SpineRuntime>,
    materialized: Option<Vec<ResponseItem>>,
}

#[derive(Debug)]
pub(crate) struct SpineToolCommit {
    pub(crate) output_text: Option<String>,
    pub(crate) record_output: bool,
    pub(crate) spine_context_already_observed: bool,
    pub(crate) deferred_history_update: Option<DeferredSpineHistoryUpdate>,
    pub(crate) deferred_tree_update: Option<SpineTreeUpdateEvent>,
}

const SPINE_COMMIT_LOCK_RETRY_LIMIT: usize = 4096;

#[derive(Debug)]
pub(crate) struct DeferredSpineHistoryUpdate {
    call_id: String,
    operation: &'static str,
    suffix_start: usize,
    expected_history: Vec<ResponseItem>,
    replacement: Vec<ResponseItem>,
    reference_context_item: Option<TurnContextItem>,
}

impl DeferredSpineHistoryUpdate {
    pub(crate) fn replace_tool_output(&mut self, item: &ResponseItem) {
        let ResponseItem::FunctionCallOutput { call_id, .. } = item else {
            return;
        };
        if call_id != &self.call_id {
            return;
        }
        if let Some(existing) = self.replacement.iter_mut().rev().find(|existing| {
            matches!(
                existing,
                ResponseItem::FunctionCallOutput {
                    call_id: existing,
                    ..
                } if existing == call_id
            )
        }) {
            *existing = item.clone();
        }
    }
}

struct SpineCommitOutput {
    snapshot: Option<SpineTreeUpdateEvent>,
    output_text: Option<String>,
    spine_context_already_observed: bool,
    history_update: Option<DeferredSpineHistoryUpdate>,
    defer_tree_update_until_raw_output: bool,
}

enum SpineCommitAttempt {
    Done(SpineCommitOutput),
    Retry,
    RuntimeMissing,
}

impl Session {
    fn no_spine_tool_commit() -> SpineToolCommit {
        SpineToolCommit {
            output_text: None,
            record_output: true,
            spine_context_already_observed: false,
            deferred_history_update: None,
            deferred_tree_update: None,
        }
    }

    fn skip_spine_tool_output_commit() -> SpineToolCommit {
        SpineToolCommit {
            output_text: None,
            record_output: false,
            spine_context_already_observed: false,
            deferred_history_update: None,
            deferred_tree_update: None,
        }
    }

    fn spine_tool_commit_from_commit_output(output: SpineCommitOutput) -> SpineToolCommit {
        SpineToolCommit {
            output_text: output.output_text,
            record_output: true,
            spine_context_already_observed: output.spine_context_already_observed,
            deferred_history_update: output.history_update,
            deferred_tree_update: output.snapshot,
        }
    }

    pub(crate) async fn send_spine_tree_update(
        &self,
        turn_context: &TurnContext,
        snapshot: SpineTreeUpdateEvent,
    ) {
        self.send_event(turn_context, EventMsg::SpineTreeUpdate(snapshot))
            .await;
    }

    pub(crate) async fn emit_initial_spine_tree_snapshot_if_needed(
        &self,
        turn_context: &TurnContext,
    ) -> Result<(), SpineError> {
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

    pub(crate) async fn seed_spine_tree_snapshot_if_available(&self) -> Result<(), SpineError> {
        let Some(spine_slot) = self.spine.as_ref() else {
            return Ok(());
        };
        let token_info = self.token_usage_info().await;
        // Host UI projection only: seeding the TUI snapshot must not mutate
        // ContextManager, ParseStack, or sidecar state.
        let snapshot = {
            let guard = spine_slot.lock().await;
            guard.ensure_valid()?;
            let Some(runtime) = guard.runtime() else {
                return Ok(());
            };
            build_annotated_tree_snapshot(runtime, token_info.as_ref())?
        };
        self.send_event_raw(Event {
            id: INITIAL_SUBMIT_ID.to_string(),
            msg: EventMsg::SpineTreeUpdate(snapshot),
        })
        .await;
        Ok(())
    }

    pub(super) async fn initialize_spine_for_new_session(&self) -> Result<(), SpineError> {
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
        guard.ensure_runtime(&rollout_path)?;
        if let Some(runtime) = guard.runtime() {
            runtime.checkpoint_initial(&rollout_path, &[])?;
        }
        Ok(())
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
        let raw_live = raw_items.iter().map(Option::is_some).collect::<Vec<_>>();
        SpineStore::clone_for_rollout_with_raw_live(boundary, &target_rollout_path, &raw_live)?;
        let raw_ordinal_limit = usize::try_from(boundary.raw_ordinal_limit()).map_err(|_| {
            SpineError::InvalidEvent("clone raw ordinal boundary overflow".to_string())
        })?;
        if raw_ordinal_limit > raw_items.len() {
            return Err(SpineError::InvalidEvent(
                "clone raw ordinal boundary exceeds fork raw length".to_string(),
            ));
        }
        if raw_ordinal_limit == raw_items.len() {
            return Ok(());
        }
        let prefix_runtime = SpineRuntime::load_for_rollout_items(
            &target_rollout_path,
            &raw_items[..raw_ordinal_limit],
            &[],
        )?;
        let mut runtime = prefix_runtime.ok_or_else(|| {
            SpineError::InvalidStore("cloned Spine sidecar is missing after fork clone".to_string())
        })?;
        for (raw_ordinal, item) in raw_items.iter().enumerate().skip(raw_ordinal_limit) {
            runtime.observe_raw_items(1)?;
            let Some(item) = item.as_ref() else {
                continue;
            };
            let context_index = runtime.materialize_history(raw_items)?.len();
            runtime.observe_context_item(
                u64::try_from(raw_ordinal)
                    .map_err(|_| SpineError::InvalidEvent("raw ordinal overflow".to_string()))?,
                context_index,
                item,
            )?;
        }
        spine_slot.lock().await.set_replayed(
            u64::try_from(raw_items.len())
                .map_err(|_| SpineError::InvalidEvent("raw item count overflow".to_string()))?,
            Some(runtime),
        )?;
        Ok(())
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
        let Some(rollout_path) = self
            .current_rollout_path()
            .await
            .map_err(|err| SpineError::InvalidStore(err.to_string()))?
        else {
            return Ok(None);
        };
        let raw_len = u64::try_from(raw_items.len())
            .map_err(|_| SpineError::InvalidEvent("raw item count overflow".to_string()))?;
        let runtime =
            SpineRuntime::load_for_rollout_items(&rollout_path, raw_items, rollback_cuts)?;
        if runtime.is_none() && (used_replacement_history || raw_items.iter().any(Option::is_some))
        {
            return Err(SpineError::InvalidStore(
                "spine_jit resume requires Spine sidecar".to_string(),
            ));
        }
        let materialized = runtime
            .as_ref()
            .map(|runtime| runtime.materialize_history(raw_items))
            .transpose()?;
        let live_root_compacts = runtime
            .as_ref()
            .map(|runtime| runtime.live_root_compacts())
            .transpose()?
            .unwrap_or_default();
        if used_replacement_history {
            let store = SpineStore::for_rollout(&rollout_path)?;
            let raw_live = raw_items.iter().map(Option::is_some).collect::<Vec<_>>();
            let Some(base_boundary) = base_replacement_history_boundary else {
                return Err(SpineError::InvalidStore(
                    "spine_jit resume used replacement_history without rollout compact boundary proof"
                        .to_string(),
                ));
            };
            store.validate_compact_checkpoint_for_boundary(
                &rollout_path,
                &raw_live,
                raw_items,
                base_boundary.raw_boundary,
                &base_boundary.replacement_history,
            )?;
            validate_live_root_compacts_have_rollout_boundary_proofs(
                &live_root_compacts,
                replacement_history_boundaries,
                &store,
                &rollout_path,
                &raw_live,
                raw_items,
            )?;
        } else {
            validate_no_live_root_compacts_without_rollout_boundaries(&live_root_compacts)?;
        }
        Ok(Some(PreparedSpineReplay {
            raw_len,
            runtime,
            materialized,
        }))
    }

    pub(super) async fn install_prepared_spine_replay(
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

    pub(super) async fn observe_spine_raw_items(&self, count: usize) -> Result<(), SpineError> {
        let Some(spine_slot) = self.spine.as_ref() else {
            return Ok(());
        };
        spine_slot.lock().await.observe_raw_items(count)
    }

    pub(super) async fn repair_spine_pressure_after_token_usage(
        &self,
        token_info: &TokenUsageInfo,
    ) {
        let Some(spine_slot) = self.spine.as_ref() else {
            return;
        };
        let current_context_tokens = token_info.last_token_usage.tokens_in_context_window();
        if current_context_tokens <= 0 {
            return;
        }
        let history = self.clone_history().await;
        let estimated_live_suffix_tokens = {
            let guard = spine_slot.lock().await;
            if guard.ensure_valid().is_err() {
                return;
            }
            let Some(runtime) = guard.runtime() else {
                return;
            };
            let Ok(open_index) = runtime.current_open_index() else {
                return;
            };
            Some(estimate_history_suffix_tokens(&history, open_index))
        };
        let result = {
            let mut guard = spine_slot.lock().await;
            if let Err(err) = guard.ensure_valid() {
                tracing::debug!("skipping Spine pressure repair: {err}");
                return;
            }
            let Some(runtime) = guard.runtime_mut() else {
                return;
            };
            runtime.ensure_current_open_context_baseline(
                current_context_tokens,
                Some(token_info.last_token_usage.input_tokens),
                estimated_live_suffix_tokens,
                history.raw_items().len(),
            )
        };
        if let Err(err) = result {
            tracing::debug!("failed to append Spine pressure repair metadata: {err}");
        }
    }

    pub(super) async fn emit_spine_tree_snapshot_cache_only_if_available(&self) {
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
            let Some(runtime) = guard.runtime() else {
                return;
            };
            match build_annotated_tree_snapshot(runtime, token_info.as_ref()) {
                Ok(snapshot) => snapshot,
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

    async fn abort_spine_pending_and_observe_completed_toolcall(
        &self,
        call_id: &str,
        completed_toolcall: CompletedToolCall,
        reason: &str,
    ) -> Result<bool, SpineError> {
        let Some(spine_slot) = self.spine.as_ref() else {
            return Ok(false);
        };
        let mut guard = spine_slot.lock().await;
        guard.ensure_valid()?;
        let Some(spine) = guard.runtime_mut() else {
            return Ok(false);
        };
        let observed =
            spine.abort_pending_and_observe_completed_toolcall(call_id, completed_toolcall)?;
        if observed {
            tracing::debug!(
                call_id,
                reason,
                "aborted pending Spine transition and kept completed toolcall as ordinary history"
            );
        }
        Ok(observed)
    }

    async fn commit_completed_toolcall_as_ordinary(
        &self,
        call_id: &str,
        completed_toolcall: CompletedToolCall,
        reason: &str,
    ) -> Result<SpineCommitOutput, SpineError> {
        let Some(spine_slot) = self.spine.as_ref() else {
            return Err(SpineError::Invariant(
                "spine slot missing during ordinary completed toolcall commit".to_string(),
            ));
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
                SpineError::InvalidStore(
                    "spine ordinary completed toolcall commit requires rollout path".to_string(),
                )
            })?;
        let rollout_history = crate::rollout::RolloutRecorder::get_rollout_history(&rollout_path)
            .await
            .map_err(|err| SpineError::InvalidStore(err.to_string()))?;
        let raw_items = spine_raw_items_after_rollback(&rollout_history.get_rollout_items());
        let (aborted_pending, materialized, snapshot) = {
            let mut guard = spine_slot.lock().await;
            guard.ensure_valid()?;
            let Some(spine) = guard.runtime_mut() else {
                return Err(SpineError::Invariant(
                    "spine runtime missing during ordinary completed toolcall commit".to_string(),
                ));
            };
            let aborted_pending =
                spine.commit_completed_toolcall_as_ordinary(call_id, completed_toolcall)?;
            let materialized = spine.materialize_history(&raw_items)?;
            let snapshot = spine.build_tree_snapshot()?;
            (aborted_pending, materialized, snapshot)
        };
        self.replace_history(materialized, self.reference_context_item().await)
            .await;
        tracing::debug!(
            call_id,
            reason,
            aborted_pending,
            "kept completed Spine toolcall as ordinary history"
        );
        Ok(SpineCommitOutput {
            snapshot: Some(snapshot),
            output_text: None,
            spine_context_already_observed: true,
            history_update: None,
            defer_tree_update_until_raw_output: false,
        })
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
        let mut guard = spine_slot.lock().await;
        guard.ensure_valid()?;
        let Some(runtime) = guard.runtime_mut() else {
            return Ok(());
        };
        let mut recorded_tool_outputs = Vec::<(String, u64, usize)>::new();
        for append in appends {
            let raw_ordinal = raw_ordinals
                .get(append.input_index)
                .copied()
                .flatten()
                .ok_or_else(|| {
                    SpineError::InvalidEvent(
                        "context append has no persisted raw ordinal".to_string(),
                    )
                })?;
            let item = items.get(append.input_index).ok_or_else(|| {
                SpineError::InvalidEvent("context append input index outside items".to_string())
            })?;
            if crate::spine::is_user_message(item) {
                let raw_end = usize::try_from(raw_ordinal)
                    .map_err(|_| SpineError::InvalidEvent("raw ordinal overflow".to_string()))?;
                let prefix = raw_items.get(..raw_end).ok_or_else(|| {
                    SpineError::InvalidEvent(
                        "checkpoint raw ordinal outside raw history".to_string(),
                    )
                })?;
                let context = runtime.materialize_history(prefix)?;
                runtime.checkpoint_before_user_msg(&rollout_path, raw_ordinal, &context)?;
            }
            runtime.observe_context_item(raw_ordinal, append.context_index, item)?;
            if let Some(call_id) = tool_response_call_id(item) {
                recorded_tool_outputs.push((
                    call_id.to_string(),
                    raw_ordinal,
                    append.context_index,
                ));
            }
        }
        runtime.observe_recorded_tool_output_group_as_completed_toolcall(&recorded_tool_outputs)?;
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

    pub(crate) async fn spine_tree(&self) -> Result<String, SpineError> {
        let spine = self.ensure_spine_runtime().await?;
        let token_info = self.token_usage_info().await;
        let guard = spine.lock().await;
        guard.ensure_valid()?;
        let Some(runtime) = guard.runtime() else {
            return Err(SpineError::InvalidStore(
                "spine runtime missing after initialization".to_string(),
            ));
        };
        render_spine_tree_for_model(runtime, token_info)
    }

    pub(crate) async fn emit_spine_tree_snapshot(
        &self,
        turn_context: &TurnContext,
    ) -> Result<(), SpineError> {
        let spine = self.ensure_spine_runtime().await?;
        let token_info = self.token_usage_info().await;
        let snapshot = {
            let guard = spine.lock().await;
            guard.ensure_valid()?;
            let Some(runtime) = guard.runtime() else {
                return Err(SpineError::InvalidStore(
                    "spine runtime missing after initialization".to_string(),
                ));
            };
            build_annotated_tree_snapshot(runtime, token_info.as_ref())?
        };
        self.send_spine_tree_update(turn_context, snapshot).await;
        Ok(())
    }

    pub(crate) async fn stage_spine_open(
        &self,
        call_id: String,
        summary: String,
    ) -> Result<(), SpineError> {
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

    pub(crate) async fn stage_spine_close(
        &self,
        call_id: String,
        instruction: Option<String>,
    ) -> Result<(), SpineError> {
        let spine = self.ensure_spine_runtime().await?;
        let mut guard = spine.lock().await;
        guard.ensure_valid()?;
        let Some(runtime) = guard.runtime_mut() else {
            return Err(SpineError::InvalidStore(
                "spine runtime missing after initialization".to_string(),
            ));
        };
        runtime.stage_close(call_id, instruction)
    }

    pub(crate) async fn stage_spine_next(
        &self,
        call_id: String,
        summary: String,
        instruction: Option<String>,
    ) -> Result<(), SpineError> {
        let spine = self.ensure_spine_runtime().await?;
        let mut guard = spine.lock().await;
        guard.ensure_valid()?;
        let Some(runtime) = guard.runtime_mut() else {
            return Err(SpineError::InvalidStore(
                "spine runtime missing after initialization".to_string(),
            ));
        };
        runtime.stage_next(call_id, summary, instruction)
    }

    #[cfg(test)]
    pub(crate) async fn maybe_commit_spine_tool_output(
        self: &Arc<Self>,
        turn_context: &Arc<TurnContext>,
        item: &ResponseItem,
    ) -> Result<SpineToolCommit, SpineError> {
        let mut client_session = self.services.model_client.new_session();
        let close_compact_tools = self
            .test_default_close_compact_tools(turn_context)
            .await
            .map_err(|err| {
                SpineError::Operation(format!("failed to build test close compact tools: {err}"))
            })?;
        self.maybe_commit_spine_tool_output_with_client_session(
            turn_context,
            &mut client_session,
            item,
            close_compact_tools,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn test_default_close_compact_tools(
        &self,
        turn_context: &Arc<TurnContext>,
    ) -> codex_protocol::error::Result<Vec<ToolSpec>> {
        Ok(built_tools(
            self,
            turn_context,
            &[],
            &HashSet::new(),
            Some(turn_context.turn_skills.outcome.as_ref()),
            &CancellationToken::new(),
        )
        .await?
        .model_visible_specs())
    }

    pub(crate) async fn maybe_commit_spine_tool_output_with_client_session(
        self: &Arc<Self>,
        turn_context: &Arc<TurnContext>,
        client_session: &mut ModelClientSession,
        item: &ResponseItem,
        close_compact_tools: Vec<ToolSpec>,
    ) -> Result<SpineToolCommit, SpineError> {
        let Some(call_id) = tool_response_call_id(item) else {
            return Ok(Self::no_spine_tool_commit());
        };
        let Some(spine_slot) = self.spine.as_ref() else {
            return Ok(Self::no_spine_tool_commit());
        };
        let raw_len = {
            let guard = spine_slot.lock().await;
            guard.ensure_valid()?;
            let Some(_spine) = guard.runtime() else {
                return Ok(Self::no_spine_tool_commit());
            };
            guard.raw_len()
        };
        let history_for_output_anchor = self.clone_history().await;
        let history_items_for_output_anchor = history_for_output_anchor.raw_items();
        let (tool_resp_raw_ordinal, tool_resp_context_index, tool_resp_already_recorded) =
            if history_items_for_output_anchor.last() == Some(item) && raw_len > 0 {
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
                    true,
                )
            } else {
                (raw_len, history_items_for_output_anchor.len(), false)
            };
        let request_anchor = {
            let guard = spine_slot.lock().await;
            guard.ensure_valid()?;
            let Some(spine) = guard.runtime() else {
                return Ok(Self::no_spine_tool_commit());
            };
            spine.pending_tool_request_anchor(call_id)?
        };
        let completed_toolcall = CompletedToolCall {
            call_id: call_id.to_string(),
            request_call_ids: vec![call_id.to_string()],
            segments: vec![
                CompletedToolCallSegment {
                    kind: ToolCallSegmentKind::Request,
                    raw_ordinal: request_anchor.raw_ordinal,
                    context_index: request_anchor.context_index,
                },
                CompletedToolCallSegment {
                    kind: ToolCallSegmentKind::Response,
                    raw_ordinal: tool_resp_raw_ordinal,
                    context_index: tool_resp_context_index,
                },
            ],
        };
        self.commit_spine_completed_toolcall_with_client_session(
            turn_context,
            client_session,
            call_id,
            item,
            completed_toolcall,
            tool_resp_already_recorded,
            close_compact_tools,
        )
        .await
    }

    pub(crate) async fn record_spine_toolcall_group_outputs_and_commit_with_client_session(
        self: &Arc<Self>,
        turn_context: &Arc<TurnContext>,
        client_session: &mut ModelClientSession,
        commit_call_id: &str,
        tool_call_ids: &[String],
        output_items: &[ResponseItem],
        close_compact_tools: Vec<ToolSpec>,
    ) -> Result<SpineToolCommit, SpineError> {
        let Some(spine_slot) = self.spine.as_ref() else {
            return Ok(Self::no_spine_tool_commit());
        };
        let expected_call_ids = tool_call_ids
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        let mut output_call_ids = std::collections::BTreeSet::new();
        for item in output_items {
            let Some(call_id) = tool_response_call_id(item) else {
                return Err(SpineError::InvalidEvent(
                    "grouped Spine toolcall output item is not a tool response".to_string(),
                ));
            };
            if !expected_call_ids.contains(call_id) {
                return Err(SpineError::InvalidEvent(format!(
                    "grouped Spine toolcall unexpected output for call_id={call_id}"
                )));
            }
            output_call_ids.insert(call_id.to_string());
        }
        for call_id in tool_call_ids {
            if !output_call_ids.contains(call_id) {
                return Err(SpineError::InvalidEvent(format!(
                    "grouped Spine toolcall missing output for call_id={call_id}"
                )));
            }
        }
        let commit_output = output_items
            .iter()
            .find(|item| tool_response_call_id(item) == Some(commit_call_id))
            .ok_or_else(|| {
                SpineError::InvalidEvent(format!(
                    "grouped Spine toolcall missing output for commit call_id={commit_call_id}"
                ))
            })?;
        let request_anchors = {
            let guard = spine_slot.lock().await;
            guard.ensure_valid()?;
            let Some(spine) = guard.runtime() else {
                return Ok(Self::no_spine_tool_commit());
            };
            tool_call_ids
                .iter()
                .map(|call_id| spine.pending_tool_request_anchor(call_id))
                .collect::<Result<Vec<_>, SpineError>>()?
        };
        let (output_raw_ordinals, _) = {
            let raw_start = spine_slot.lock().await.raw_len();
            assign_spine_raw_ordinals(raw_start, output_items)?
        };
        let output_context_start = self.clone_history().await.raw_items().len();
        self.record_conversation_items_without_spine_observe(turn_context, output_items)
            .await
            .map_err(|err| {
                SpineError::Operation(format!(
                    "failed to record grouped Spine tool outputs before commit: {err}"
                ))
            })?;
        let mut request_segments = request_anchors
            .iter()
            .map(|anchor| CompletedToolCallSegment {
                kind: ToolCallSegmentKind::Request,
                raw_ordinal: anchor.raw_ordinal,
                context_index: anchor.context_index,
            })
            .collect::<Vec<_>>();
        request_segments.sort_by_key(|segment| (segment.context_index, segment.raw_ordinal));
        let mut response_segments = Vec::new();
        for (index, raw_ordinal) in output_raw_ordinals.iter().enumerate() {
            let Some(raw_ordinal) = raw_ordinal else {
                continue;
            };
            response_segments.push(CompletedToolCallSegment {
                kind: ToolCallSegmentKind::Response,
                raw_ordinal: *raw_ordinal,
                context_index: output_context_start + index,
            });
        }
        response_segments.sort_by_key(|segment| (segment.context_index, segment.raw_ordinal));
        if request_segments.is_empty() {
            return Err(SpineError::InvalidEvent(
                "completed grouped toolcall must contain at least one request".to_string(),
            ));
        }
        if response_segments.is_empty() {
            return Err(SpineError::InvalidEvent(
                "completed grouped toolcall must contain at least one response".to_string(),
            ));
        }
        let mut group_segments =
            Vec::with_capacity(request_segments.len() + response_segments.len());
        group_segments.extend(request_segments);
        group_segments.extend(response_segments);
        let completed_toolcall = CompletedToolCall {
            call_id: commit_call_id.to_string(),
            request_call_ids: tool_call_ids.to_vec(),
            segments: group_segments,
        };
        self.commit_spine_completed_toolcall_with_client_session(
            turn_context,
            client_session,
            commit_call_id,
            commit_output,
            completed_toolcall,
            true,
            close_compact_tools,
        )
        .await
    }

    async fn commit_spine_completed_toolcall_with_client_session(
        self: &Arc<Self>,
        turn_context: &Arc<TurnContext>,
        client_session: &mut ModelClientSession,
        call_id: &str,
        item: &ResponseItem,
        completed_toolcall: CompletedToolCall,
        tool_resp_already_recorded: bool,
        close_compact_tools: Vec<ToolSpec>,
    ) -> Result<SpineToolCommit, SpineError> {
        let Some(spine_slot) = self.spine.as_ref() else {
            return Ok(Self::no_spine_tool_commit());
        };
        let pending_commit = {
            let guard = spine_slot.lock().await;
            guard.ensure_valid()?;
            let Some(spine) = guard.runtime() else {
                return Ok(Self::no_spine_tool_commit());
            };
            spine.pending_commit(call_id)?
        };
        let pre_compact_token_baselines =
            if matches!(pending_commit, Some(SpinePendingCommit::Close { .. })) {
                Some(token_baselines_from_info(
                    self.token_usage_info().await.as_ref(),
                ))
            } else {
                None
            };
        let close_compact = match pending_commit {
            Some(SpinePendingCommit::Close {
                action,
                node,
                suffix_start,
                instruction,
                next_summary,
            }) => {
                let history = self.clone_history().await;
                let expected_history = history.raw_items().to_vec();
                let toolcall_start = completed_toolcall
                    .segments
                    .first()
                    .map(|segment| segment.context_index)
                    .ok_or_else(|| {
                        SpineError::InvalidEvent(
                            "completed toolcall missing first segment".to_string(),
                        )
                    })?;
                let source_context = {
                    let guard = spine_slot.lock().await;
                    guard.ensure_valid()?;
                    let Some(spine) = guard.runtime() else {
                        return Err(SpineError::Invariant(format!(
                            "spine runtime missing while building close compact source plan for call_id={call_id}"
                        )));
                    };
                    build_spine_close_target_projection(
                        spine,
                        &node,
                        action,
                        next_summary.as_deref(),
                    )
                    .and_then(|close_target_projection| {
                        let source_plan = spine.build_close_source_plan(
                            history.raw_items(),
                            &node,
                            suffix_start,
                            toolcall_start,
                            call_id,
                        )?;
                        Ok((close_target_projection, source_plan))
                    })
                };
                let (close_target_projection, source_plan): (String, SpineCompactSourcePlan) =
                    match source_context {
                        Ok(source_context) => source_context,
                        Err(err) => {
                            let reason = "spine close source plan failed before compact";
                            if tool_resp_already_recorded {
                                self.fail_closed_spine_toolcall_commit(call_id, reason)
                                    .await;
                            } else {
                                self.abort_spine_pending_tool(call_id, reason).await;
                            }
                            return Err(err);
                        }
                    };
                let compact = match Box::pin(self.spine_compact_close(
                    turn_context,
                    client_session,
                    &history,
                    node.to_string(),
                    suffix_start,
                    item,
                    instruction,
                    close_target_projection,
                    source_plan,
                    close_compact_tools,
                ))
                .await
                {
                    Ok(SpineCloseCompactOutcome::Compact(compact)) => compact,
                    Ok(SpineCloseCompactOutcome::NativeCompacted {
                        reset_client_session,
                    }) => {
                        if reset_client_session {
                            client_session.reset_websocket_session();
                        }
                        let reason = "spine close invalidated by native compact";
                        if tool_resp_already_recorded {
                            let commit_output = self
                                .commit_completed_toolcall_as_ordinary(
                                    call_id,
                                    completed_toolcall.clone(),
                                    reason,
                                )
                                .await?;
                            return Ok(Self::spine_tool_commit_from_commit_output(commit_output));
                        }
                        self.abort_spine_pending_tool(call_id, reason).await;
                        return Ok(Self::skip_spine_tool_output_commit());
                    }
                    Err(err) => {
                        let reason = "spine close compact failed before commit";
                        if tool_resp_already_recorded {
                            self.abort_spine_pending_and_observe_completed_toolcall(
                                call_id,
                                completed_toolcall.clone(),
                                reason,
                            )
                            .await?;
                        } else {
                            self.abort_spine_pending_tool(call_id, reason).await;
                        }
                        return Err(err);
                    }
                };
                Some((compact, expected_history))
            }
            Some(SpinePendingCommit::Open) | None => None,
        };
        if let Some((_, expected_history)) = close_compact.as_ref() {
            let history = self.clone_history().await;
            if history.raw_items() != expected_history.as_slice() {
                let reason = "spine close history changed before commit";
                if tool_resp_already_recorded {
                    self.fail_closed_spine_toolcall_commit(call_id, reason)
                        .await;
                } else {
                    self.abort_spine_pending_tool(call_id, reason).await;
                }
                return Err(SpineError::Operation(format!(
                    "spine.close history changed while compacting suffix for call_id={call_id}"
                )));
            }
        }
        let mut lock_retries = 0;
        let commit_output = loop {
            match self.try_commit_spine_tool_output_once(
                spine_slot,
                call_id,
                item,
                close_compact.clone(),
                pre_compact_token_baselines,
                completed_toolcall.clone(),
                tool_resp_already_recorded,
            )? {
                SpineCommitAttempt::Done(output) => break output,
                SpineCommitAttempt::RuntimeMissing => {
                    let reason = "spine runtime missing during completed toolcall commit";
                    if tool_resp_already_recorded {
                        self.fail_closed_spine_toolcall_commit(call_id, reason)
                            .await;
                        return Err(SpineError::Invariant(format!(
                            "{reason} for call_id={call_id}"
                        )));
                    }
                    return Ok(Self::no_spine_tool_commit());
                }
                SpineCommitAttempt::Retry if lock_retries < SPINE_COMMIT_LOCK_RETRY_LIMIT => {
                    lock_retries += 1;
                    tokio::task::yield_now().await;
                }
                SpineCommitAttempt::Retry => {
                    let reason = "spine tool output commit lock retry limit exceeded before commit";
                    if tool_resp_already_recorded {
                        self.fail_closed_spine_toolcall_commit(call_id, reason)
                            .await;
                    } else {
                        self.abort_spine_pending_tool(call_id, reason).await;
                    }
                    return Err(SpineError::Operation(format!(
                        "spine tool output commit could not acquire session locks after {SPINE_COMMIT_LOCK_RETRY_LIMIT} retries for call_id={call_id}"
                    )));
                }
            }
        };
        let SpineCommitOutput {
            snapshot,
            output_text,
            spine_context_already_observed,
            history_update,
            defer_tree_update_until_raw_output,
        } = commit_output;
        let deferred_tree_update = if defer_tree_update_until_raw_output {
            snapshot
        } else {
            if let Some(snapshot) = snapshot {
                self.send_spine_tree_update(turn_context, snapshot).await;
            }
            None
        };
        if deferred_tree_update.is_some() {
            tracing::debug!(
                call_id,
                "deferring Spine close-like tree update until raw output evidence is durable"
            );
        }
        Ok(SpineToolCommit {
            output_text,
            record_output: true,
            spine_context_already_observed,
            deferred_history_update: history_update,
            deferred_tree_update,
        })
    }

    pub(crate) async fn apply_deferred_spine_history_update(
        &self,
        update: DeferredSpineHistoryUpdate,
    ) -> CodexResult<()> {
        let result = {
            let mut state = self.state.lock().await;
            let history = state.clone_history();
            let current_history = history.raw_items();
            if current_history != update.expected_history.as_slice() {
                Err(format!(
                    "{} history changed before deferred suffix replacement for call_id={}",
                    update.operation, update.call_id
                ))
            } else if update.suffix_start > current_history.len() {
                Err(format!(
                    "{} suffix start {} exceeds history length {} for call_id={}",
                    update.operation,
                    update.suffix_start,
                    current_history.len(),
                    update.call_id
                ))
            } else {
                state
                    .replace_history_suffix(
                        update.suffix_start..current_history.len(),
                        update.replacement,
                        update.reference_context_item,
                    )
                    .map_err(|err| {
                        format!(
                            "{} deferred suffix replacement failed for call_id={}: {err}",
                            update.operation, update.call_id
                        )
                    })
            }
        };
        if let Err(reason) = result {
            self.invalidate_spine_runtime(reason.clone()).await;
            return Err(CodexErr::SpineTerminalFailure {
                operation: "apply deferred Spine close-like history replacement".to_string(),
                reason,
            });
        }
        Ok(())
    }

    fn try_commit_spine_tool_output_once(
        &self,
        spine_slot: &Mutex<SpineSessionState>,
        call_id: &str,
        tool_resp_item: &ResponseItem,
        close_compact: Option<(SpineCloseCompact, Vec<ResponseItem>)>,
        pre_compact_token_baselines: Option<SpineTokenBaselines>,
        completed_toolcall: CompletedToolCall,
        tool_resp_already_recorded: bool,
    ) -> Result<SpineCommitAttempt, SpineError> {
        let Ok(mut guard) = spine_slot.try_lock() else {
            return Ok(SpineCommitAttempt::Retry);
        };
        guard.ensure_valid()?;
        let Some(spine) = guard.runtime_mut() else {
            return Ok(SpineCommitAttempt::RuntimeMissing);
        };
        let Ok(state) = self.state.try_lock() else {
            return Ok(SpineCommitAttempt::Retry);
        };
        if let Some((_, expected_history)) = close_compact.as_ref()
            && state.clone_history().raw_items() != expected_history.as_slice()
        {
            if spine.abort_pending(call_id) {
                tracing::debug!(
                    call_id,
                    reason = "spine close history changed before suffix replacement",
                    "aborted pending Spine transition"
                );
            }
            return Err(SpineError::Operation(format!(
                "spine.close history changed before suffix replacement for call_id={call_id}"
            )));
        }
        let close_compact = close_compact.map(|(compact, _)| compact);
        let token_baselines = pre_compact_token_baselines
            .unwrap_or_else(|| token_baselines_from_info(state.token_info().as_ref()));
        let pending_commit = spine.pending_commit(call_id)?;
        let commit_kind = if pending_commit.is_some() {
            spine.maybe_commit_output_with_toolcall(
                call_id,
                close_compact,
                token_baselines,
                completed_toolcall,
            )?
        } else {
            spine.observe_completed_toolcall(completed_toolcall)?;
            None
        };
        let defer_tree_update_until_raw_output = matches!(
            commit_kind,
            Some(SpineCommitKind::Close { .. } | SpineCommitKind::CloseThenOpen { .. })
        );
        let mut snapshot = None;
        let mut output_text = None;
        let mut history_update = None;
        if let Some(commit_kind) = commit_kind.as_ref() {
            let output_prefix = match commit_kind {
                SpineCommitKind::Open { .. } => "Spine opened.",
                SpineCommitKind::Close { .. } => "Spine closed.",
                SpineCommitKind::CloseThenOpen { .. } => "Spine advanced to next sibling.",
            };
            match commit_kind {
                SpineCommitKind::Open { open_request_index } => {
                    let history = state.clone_history();
                    let history_items = history.raw_items();
                    if *open_request_index > history_items.len() {
                        return Err(SpineError::Invariant(format!(
                            "spine.open request index {open_request_index} exceeds history length {} for call_id={call_id}",
                            history_items.len()
                        )));
                    }
                }
                SpineCommitKind::Close {
                    suffix_start,
                    replacement,
                    toolcall_start,
                } => {
                    let history = state.clone_history();
                    let history_items = history.raw_items();
                    let suffix_end = history_items.len();
                    if *suffix_start > suffix_end {
                        return Err(SpineError::Invariant(format!(
                            "spine.close suffix start {suffix_start} exceeds history length {suffix_end} for call_id={call_id}"
                        )));
                    }
                    if *toolcall_start > suffix_end {
                        return Err(SpineError::Invariant(format!(
                            "spine.close toolcall start {toolcall_start} exceeds history length {suffix_end} for call_id={call_id}"
                        )));
                    }
                    let mut replacement = replacement.clone();
                    replacement.extend_from_slice(&history_items[*toolcall_start..]);
                    if !tool_resp_already_recorded {
                        replacement.push(tool_resp_item.clone());
                    }
                    history_update = Some(DeferredSpineHistoryUpdate {
                        call_id: call_id.to_string(),
                        operation: "spine.close",
                        suffix_start: *suffix_start,
                        expected_history: history_items.to_vec(),
                        replacement,
                        reference_context_item: state.reference_context_item(),
                    });
                }
                SpineCommitKind::CloseThenOpen {
                    suffix_start,
                    replacement,
                    toolcall_start,
                    ..
                } => {
                    let history = state.clone_history();
                    let history_items = history.raw_items();
                    let suffix_end = history_items.len();
                    if *suffix_start > suffix_end {
                        return Err(SpineError::Invariant(format!(
                            "spine.next suffix start {suffix_start} exceeds history length {suffix_end} for call_id={call_id}"
                        )));
                    }
                    if *toolcall_start > suffix_end {
                        return Err(SpineError::Invariant(format!(
                            "spine.next toolcall start {toolcall_start} exceeds history length {suffix_end} for call_id={call_id}"
                        )));
                    }
                    let mut replacement = replacement.clone();
                    replacement.extend_from_slice(&history_items[*toolcall_start..]);
                    if !tool_resp_already_recorded {
                        replacement.push(tool_resp_item.clone());
                    }
                    history_update = Some(DeferredSpineHistoryUpdate {
                        call_id: call_id.to_string(),
                        operation: "spine.next",
                        suffix_start: *suffix_start,
                        expected_history: history_items.to_vec(),
                        replacement,
                        reference_context_item: state.reference_context_item(),
                    });
                }
            }
            let token_info = state.token_info();
            snapshot = Some(build_annotated_tree_snapshot(spine, token_info.as_ref())?);
            let tree = render_spine_tree_for_model(spine, token_info)?;
            output_text = Some(format!("{output_prefix}\n\n{tree}"));
        }
        Ok(SpineCommitAttempt::Done(SpineCommitOutput {
            snapshot,
            output_text,
            spine_context_already_observed: true,
            history_update,
            defer_tree_update_until_raw_output,
        }))
    }

    pub(crate) async fn is_pending_spine_close_like_output(
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
        let Some(spine) = guard.runtime() else {
            return Ok(false);
        };
        Ok(matches!(
            spine.pending_commit(call_id)?,
            Some(SpinePendingCommit::Close { .. })
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
        let Some(spine) = guard.runtime() else {
            return Ok(false);
        };
        Ok(spine.is_control_output_call_id(call_id))
    }

    #[cfg(test)]
    pub(crate) async fn install_spine_root_compact(
        &self,
        body: String,
    ) -> Result<Option<(SpineRootCompactResult, SpineTreeUpdateEvent)>, SpineError> {
        self.install_spine_root_compact_with_handoff(body, None)
            .await
    }

    async fn install_spine_root_compact_with_handoff(
        &self,
        body: String,
        root_compact_handoff: Option<SpineTokenBaselines>,
    ) -> Result<Option<(SpineRootCompactResult, SpineTreeUpdateEvent)>, SpineError> {
        let Some(spine_slot) = self.spine.as_ref() else {
            return Ok(None);
        };
        {
            let guard = spine_slot.lock().await;
            guard.ensure_valid()?;
            if guard.runtime().is_none() {
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
        let close_baselines = token_baselines_from_info(self.token_usage_info().await.as_ref());
        let token_metadata = SpineRootCompactTokenMetadata {
            close_input_tokens: close_baselines.input_tokens,
            close_context_tokens: close_baselines.context_tokens,
            next_open_input_tokens: root_compact_handoff.and_then(|handoff| handoff.input_tokens),
            next_open_context_tokens: root_compact_handoff
                .and_then(|handoff| handoff.context_tokens),
        };
        {
            let mut guard = spine_slot.lock().await;
            guard.ensure_valid()?;
            let result = match guard
                .runtime_mut()
                .ok_or_else(|| {
                    SpineError::InvalidStore(
                        "spine runtime missing after initialization".to_string(),
                    )
                })?
                .root_compact_with_checkpoint(&rollout_path, body, &raw_items, token_metadata)
            {
                Ok(result) => result,
                Err(err) => {
                    if !err.should_invalidate_runtime() {
                        tracing::debug!(
                            error_class = ?err.class(),
                            "invalidating Spine runtime after root compact failure to preserve existing fail-closed behavior"
                        );
                    }
                    guard.invalidate(format!(
                        "failed to install Spine root compact [{:?}]: {err}",
                        err.class()
                    ));
                    return Err(err);
                }
            };
            let Some(spine) = guard.runtime() else {
                return Ok(None);
            };
            let current_open_index = spine.current_open_index()?;
            if current_open_index != result.materialized.len() {
                return Err(SpineError::Invariant(format!(
                    "spine root compact open index {current_open_index} does not match materialized history length {}",
                    result.materialized.len()
                )));
            }
            let snapshot = spine.build_tree_snapshot()?;
            Ok(Some((result, snapshot)))
        }
    }

    pub(crate) async fn install_spine_root_compact_after_native_compact(
        &self,
        items: &mut Vec<ResponseItem>,
        compacted_item: &mut CompactedItem,
        root_compact_handoff: Option<SpineTokenBaselines>,
    ) -> CodexResult<Option<SpineTreeUpdateEvent>> {
        let Some(spine_slot) = self.spine.as_ref() else {
            return Ok(None);
        };
        {
            let guard = spine_slot.lock().await;
            guard
                .ensure_valid()
                .map_err(|err| CodexErr::SpineTerminalFailure {
                    operation: "install Spine root compact".to_string(),
                    reason: err.to_string(),
                })?;
            if guard.runtime().is_none() {
                return Ok(None);
            }
        }
        let body = spine_root_compact_body(items, compacted_item).ok_or_else(|| {
            CodexErr::SpineTerminalFailure {
                operation: "install Spine root compact".to_string(),
                reason: "native compact produced no model-visible Spine root memory body"
                    .to_string(),
            }
        })?;
        let Some((root_compact, snapshot)) = self
            .install_spine_root_compact_with_handoff(body, root_compact_handoff)
            .await
            .map_err(|err| CodexErr::SpineTerminalFailure {
                operation: "install Spine root compact".to_string(),
                reason: err.to_string(),
            })?
        else {
            return Ok(None);
        };
        *items = root_compact.materialized;
        compacted_item.replacement_history = Some(items.clone());
        Ok(Some(snapshot))
    }
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
            &boundary.replacement_history,
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

fn spine_root_compact_body(
    replacement_history: &[ResponseItem],
    compacted_item: &CompactedItem,
) -> Option<String> {
    // These carriers are Codex native compact success outputs. This is not a
    // legacy Spine fallback or rendered-history parser.
    let message = compacted_item.message.trim();
    if !message.is_empty() {
        return Some(compacted_item.message.clone());
    }
    compacted_item
        .replacement_history
        .as_deref()
        .and_then(crate::compact_remote::remote_root_compact_body)
        .or_else(|| crate::compact_remote::remote_root_compact_body(replacement_history))
}

fn build_annotated_tree_snapshot(
    runtime: &SpineRuntime,
    token_info: Option<&TokenUsageInfo>,
) -> Result<SpineTreeUpdateEvent, SpineError> {
    Ok(build_spine_tree_inside_view(runtime, token_info)?.snapshot)
}

fn render_spine_tree_for_model(
    runtime: &SpineRuntime,
    token_info: Option<TokenUsageInfo>,
) -> Result<String, SpineError> {
    Ok(build_spine_tree_inside_view(runtime, token_info.as_ref())?.rendered_tree)
}

fn build_spine_close_target_projection(
    runtime: &SpineRuntime,
    node: &NodeId,
    action: SpinePendingCloseAction,
    _next_summary: Option<&str>,
) -> Result<String, SpineError> {
    let snapshot = runtime.build_tree_snapshot()?;
    let closing_node_id = node.to_string();
    let closing_node = snapshot
        .nodes
        .iter()
        .find(|candidate| candidate.node_id == closing_node_id)
        .ok_or_else(|| {
            SpineError::Invariant(format!(
                "pending Spine close target {closing_node_id} is missing from pre-commit tree"
            ))
        })?;
    let parent_node = if let Some(parent_id) = closing_node.parent_id.as_deref() {
        Some(
            snapshot
                .nodes
                .iter()
                .find(|candidate| candidate.node_id == parent_id)
                .ok_or_else(|| {
                    SpineError::Invariant(format!(
                        "pending Spine close target {closing_node_id} references missing parent {parent_id} in pre-commit tree"
                    ))
                })?,
        )
    } else {
        None
    };

    let mut text = "---------- Spine Close Target ----------\n\
Use this only to orient the memory; do not summarize this block.\n"
        .to_string();
    text.push_str(match action {
        SpinePendingCloseAction::Close => "Action: close\n",
        SpinePendingCloseAction::Next => "Action: next\n",
    });
    text.push_str(&format!("Closing node: {}\n", closing_node.node_id));
    text.push_str(&format!(
        "Cursor before commit: {}\n",
        snapshot.active_node_id
    ));
    if let Some(parent) = parent_node {
        text.push_str(&format!("Parent node: {}\n", parent.node_id));
    } else {
        text.push_str("Parent node: none\n");
    }
    if action == SpinePendingCloseAction::Next {
        text.push_str("Next sibling: pending\n");
    }
    Ok(text)
}

fn token_baselines_from_info(current: Option<&TokenUsageInfo>) -> SpineTokenBaselines {
    let Some(current) = current else {
        return SpineTokenBaselines::default();
    };
    SpineTokenBaselines {
        input_tokens: Some(current.last_token_usage.input_tokens),
        context_tokens: Some(current.last_token_usage.tokens_in_context_window()),
    }
}

fn estimate_history_suffix_tokens(history: &ContextManager, open_index: usize) -> i64 {
    let bytes = history
        .raw_items()
        .get(open_index..)
        .unwrap_or(&[])
        .iter()
        .map(estimate_response_item_model_visible_bytes)
        .fold(0i64, i64::saturating_add);
    approx_tokens_from_byte_count_i64(bytes)
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

fn tool_response_call_id(item: &ResponseItem) -> Option<&str> {
    match item {
        ResponseItem::FunctionCallOutput { call_id, .. }
        | ResponseItem::CustomToolCallOutput { call_id, .. } => Some(call_id.as_str()),
        ResponseItem::ToolSearchOutput {
            call_id: Some(call_id),
            ..
        } => Some(call_id.as_str()),
        _ => None,
    }
}
