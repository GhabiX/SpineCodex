use super::*;
use crate::client::ModelClientSession;
use crate::context_manager::ContextAppend;
use crate::session::rollout_reconstruction::ReplacementHistoryBoundary;
use crate::session::spine_memory_assembly::spine_close_memory_assembly_from_tool_arg;
use crate::session::spine_tree_inside::build_spine_tree_inside_view;
use crate::spine::CompletedToolCall;
use crate::spine::CompletedToolCallSegment;
use crate::spine::IntoSpineNodeMemory;
use crate::spine::LiveRootCompact;
use crate::spine::SpineCloneBoundary;
use crate::spine::SpineCloseMemoryAssembly;
use crate::spine::SpineCommitKind;
use crate::spine::SpinePendingCommit;
use crate::spine::SpinePreparedCommit;
#[cfg(test)]
use crate::spine::SpineRootCompactResult;
use crate::spine::SpineRootCompactTokenMetadata;
use crate::spine::SpineRuntime;
use crate::spine::SpineStore;
use crate::spine::SpineTokenBaselines;
use crate::spine::SpineTrimOutcome;
use crate::spine::ToolCallSegmentKind;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ReasoningItemContent;
use codex_protocol::models::ReasoningItemReasoningSummary;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::TokenUsageInfo;
use codex_protocol::protocol::TurnContextItem;
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
pub(crate) struct SpineToolCommit {
    pub(crate) record_output: bool,
    pub(crate) spine_context_already_observed: bool,
    pub(crate) deferred_tree_update: Option<SpineTreeUpdateEvent>,
}

const SPINE_COMMIT_LOCK_RETRY_LIMIT: usize = 4096;

#[derive(Debug)]
struct SpineHistoryUpdate {
    call_id: String,
    operation: &'static str,
    suffix_start: usize,
    expected_history: Vec<ResponseItem>,
    replacement: Vec<ResponseItem>,
    reference_context_item: Option<TurnContextItem>,
}

struct SpineCommitOutput {
    snapshot: Option<SpineTreeUpdateEvent>,
    spine_context_already_observed: bool,
    defer_tree_update_until_raw_output: bool,
}

struct CompletedToolCallEvidenceParts {
    call_id: String,
    request_call_ids: Vec<String>,
    request_segments: Vec<CompletedToolCallSegment>,
    response_segments: Vec<CompletedToolCallSegment>,
    missing_request_error: &'static str,
    missing_response_error: &'static str,
}

pub(crate) struct PreparedSpineRootCompactInstall {
    prepared: crate::spine::SpinePreparedRootCompact,
}

enum SpineCommitAttempt {
    Done(SpineCommitOutput),
    Retry,
    RuntimeMissing,
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
    pub(crate) fn no_spine_tool_commit() -> SpineToolCommit {
        SpineToolCommit {
            record_output: true,
            spine_context_already_observed: false,
            deferred_tree_update: None,
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

    fn apply_spine_history_replacement_to_locked_state(
        state: &mut crate::state::SessionState,
        update: SpineHistoryUpdate,
    ) -> Result<(), String> {
        let history = state.clone_history();
        let current_history = history.raw_items();
        if current_history != update.expected_history.as_slice() {
            Err(format!(
                "{} history changed before suffix replacement for call_id={}",
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
                        "{} suffix replacement failed for call_id={}: {err}",
                        update.operation, update.call_id
                    )
                })
        }
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
            if runtime.jit_enabled() {
                runtime.checkpoint_initial(&rollout_path, &[])?;
            }
        }
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
        let history = self.clone_history().await;
        let projected = {
            let guard = spine_slot.lock().await;
            guard.ensure_valid()?;
            let Some(runtime) = guard.runtime() else {
                return Ok(());
            };
            runtime.project_raw_history_with_trim(history.raw_items())?
        };
        self.replace_history(projected, self.reference_context_item().await)
            .await;
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
            let runtime = SpineRuntime::load_for_rollout_items_for_writer_with_jit(
                &target_rollout_path,
                raw_items,
                &[],
                self.features.enabled(Feature::SpineJit),
            )?;
            spine_slot.lock().await.set_replayed(
                u64::try_from(raw_items.len())
                    .map_err(|_| SpineError::InvalidEvent("raw item count overflow".to_string()))?,
                runtime,
            )?;
            return Ok(());
        }
        let prefix_runtime = SpineRuntime::load_for_rollout_items_for_writer_with_jit(
            &target_rollout_path,
            &raw_items[..raw_ordinal_limit],
            &[],
            self.features.enabled(Feature::SpineJit),
        )?;
        let mut runtime = prefix_runtime.ok_or_else(|| {
            SpineError::InvalidStore("cloned Spine sidecar is missing after fork clone".to_string())
        })?;
        runtime.set_jit_enabled(self.features.enabled(Feature::SpineJit));
        runtime.set_trim_enabled(self.features.enabled(Feature::SpineTrim));
        let mut recorded_tool_outputs = Vec::<(String, u64, usize)>::new();
        for (raw_ordinal, item) in raw_items.iter().enumerate().skip(raw_ordinal_limit) {
            runtime.observe_raw_items(1)?;
            let Some(item) = item.as_ref() else {
                continue;
            };
            let context_index = if runtime.jit_enabled() {
                runtime.materialize_history(raw_items)?.len()
            } else {
                raw_items
                    .iter()
                    .take(raw_ordinal)
                    .filter(|item| item.is_some())
                    .count()
            };
            let raw_ordinal = u64::try_from(raw_ordinal)
                .map_err(|_| SpineError::InvalidEvent("raw ordinal overflow".to_string()))?;
            runtime.observe_context_item(raw_ordinal, context_index, item)?;
            if let Some(call_id) = tool_response_call_id(item) {
                recorded_tool_outputs.push((call_id.to_string(), raw_ordinal, context_index));
            }
        }
        runtime.observe_recorded_tool_output_group_as_completed_toolcall_with_raw_items(
            &recorded_tool_outputs,
            raw_items,
        )?;
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
        let mut runtime =
            SpineRuntime::load_for_rollout_items(&rollout_path, raw_items, rollback_cuts)?;
        if let Some(runtime) = runtime.as_mut() {
            runtime.set_jit_enabled(self.features.enabled(Feature::SpineJit));
            runtime.set_trim_enabled(self.features.enabled(Feature::SpineTrim));
        }
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

    pub(super) async fn prepare_spine_trim_replay_from_rollout_items(
        &self,
        raw_len: u64,
        history: &[ResponseItem],
    ) -> Result<Option<SpineRuntime>, SpineError> {
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
        if !SpineStore::has_for_rollout(&rollout_path)? {
            return Ok(None);
        }
        let mut runtime = SpineRuntime::load_or_create_with_jit(&rollout_path, raw_len, false)?;
        runtime.set_trim_enabled(true);
        runtime.project_raw_history_with_trim(history)?;
        Ok(Some(runtime))
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
            if runtime.jit_enabled() && crate::spine::is_real_user_message(item) {
                runtime.checkpoint_before_user_msg(&rollout_path, raw_ordinal, &raw_items)?;
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
        runtime.observe_recorded_tool_output_group_as_completed_toolcall_with_raw_items(
            &recorded_tool_outputs,
            &raw_items,
        )?;
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
        let guard = spine.lock().await;
        guard.ensure_valid()?;
        let Some(runtime) = guard.runtime() else {
            return Err(SpineError::InvalidStore(
                "spine runtime missing after initialization".to_string(),
            ));
        };
        let view = build_spine_tree_inside_view(runtime, token_info.as_ref())?;
        drop(guard);

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
            guard.ensure_valid()?;
            let Some(runtime) = guard.runtime() else {
                return Err(SpineError::InvalidStore(
                    "spine runtime missing after initialization".to_string(),
                ));
            };
            build_annotated_tree_snapshot(runtime, token_info.as_ref())?
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

    pub(crate) async fn stage_spine_close<M: IntoSpineNodeMemory>(
        &self,
        call_id: String,
        memory: M,
    ) -> Result<(), SpineError> {
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

    pub(crate) async fn stage_spine_next<M: IntoSpineNodeMemory>(
        &self,
        call_id: String,
        summary: String,
        memory: M,
    ) -> Result<(), SpineError> {
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
        let raw_items = self
            .clone_history()
            .await
            .raw_items()
            .iter()
            .cloned()
            .map(Some)
            .collect::<Vec<_>>();
        let spine = self.ensure_spine_runtime().await?;
        let mut guard = spine.lock().await;
        guard.ensure_valid()?;
        let Some(runtime) = guard.runtime_mut() else {
            return Err(SpineError::InvalidStore(
                "spine runtime missing after initialization".to_string(),
            ));
        };
        match request {
            SpineTrimRequest::Snip => runtime.trim_tool_response(&trim_id),
            SpineTrimRequest::SliceHead { head } => {
                runtime.slice_tool_response_head(&trim_id, head, &raw_items)
            }
            SpineTrimRequest::SliceTail { tail } => {
                runtime.slice_tool_response_tail(&trim_id, tail, &raw_items)
            }
            SpineTrimRequest::SliceAnchor {
                anchor,
                preceding,
                following,
            } => runtime
                .slice_tool_response_anchor(&trim_id, &anchor, preceding, following, &raw_items),
        }
    }

    pub(crate) async fn append_spine_feedback(&self, content: String) -> Result<(), SpineError> {
        let spine = self.ensure_spine_runtime().await?;
        let guard = spine.lock().await;
        guard.ensure_valid()?;
        let Some(runtime) = guard.runtime() else {
            return Err(SpineError::InvalidStore(
                "spine runtime missing after initialization".to_string(),
            ));
        };
        let entry = format!(
            "## {}\n\n{}",
            chrono::Utc::now().to_rfc3339(),
            content.trim()
        );
        runtime.append_feedback_markdown(&entry)
    }

    #[cfg(test)]
    pub(crate) async fn maybe_commit_spine_tool_output(
        self: &Arc<Self>,
        turn_context: &Arc<TurnContext>,
        item: &ResponseItem,
    ) -> Result<SpineToolCommit, SpineError> {
        let mut client_session = self.services.model_client.new_session();
        self.maybe_commit_spine_tool_output_with_client_session(
            turn_context,
            &mut client_session,
            item,
        )
        .await
    }

    pub(crate) async fn maybe_commit_spine_tool_output_with_client_session(
        self: &Arc<Self>,
        turn_context: &Arc<TurnContext>,
        client_session: &mut ModelClientSession,
        item: &ResponseItem,
    ) -> Result<SpineToolCommit, SpineError> {
        let Some(call_id) = tool_response_call_id(item) else {
            return Ok(Self::no_spine_tool_commit());
        };
        let Some(spine_slot) = self.spine.as_ref() else {
            return Ok(Self::no_spine_tool_commit());
        };
        let mut recorded_output_inside_reduce = false;
        let mut raw_len;
        let mut history_for_output_anchor;
        loop {
            raw_len = {
                let guard = spine_slot.lock().await;
                guard.ensure_valid()?;
                let Some(_spine) = guard.runtime() else {
                    return Ok(Self::no_spine_tool_commit());
                };
                guard.raw_len()
            };
            history_for_output_anchor = self.clone_history().await;
            let history_items_for_output_anchor = history_for_output_anchor.raw_items();
            let tool_resp_already_recorded =
                history_items_for_output_anchor.last() == Some(item) && raw_len > 0;
            if tool_resp_already_recorded || recorded_output_inside_reduce {
                break;
            }
            let is_close_like = {
                let guard = spine_slot.lock().await;
                guard.ensure_valid()?;
                let Some(spine) = guard.runtime() else {
                    return Ok(Self::no_spine_tool_commit());
                };
                matches!(
                    spine.pending_commit(call_id)?,
                    Some(SpinePendingCommit::Close { .. })
                )
            };
            if !is_close_like {
                break;
            }
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
        let request_anchor = {
            let guard = spine_slot.lock().await;
            guard.ensure_valid()?;
            let Some(spine) = guard.runtime() else {
                return Ok(Self::no_spine_tool_commit());
            };
            spine.pending_tool_request_anchor(call_id)?
        };
        let completed_toolcall = completed_toolcall_evidence(CompletedToolCallEvidenceParts {
            call_id: call_id.to_string(),
            request_call_ids: vec![call_id.to_string()],
            request_segments: vec![completed_toolcall_request_segment(
                request_anchor.raw_ordinal,
                request_anchor.context_index,
            )],
            response_segments: vec![completed_toolcall_response_segment(
                tool_resp_raw_ordinal,
                tool_resp_context_index,
            )],
            missing_request_error: "completed toolcall must contain a request",
            missing_response_error: "completed toolcall must contain a response",
        })?;
        self.commit_spine_completed_toolcall_with_client_session(
            turn_context,
            client_session,
            call_id,
            item,
            completed_toolcall,
            tool_resp_already_recorded,
            recorded_output_inside_reduce,
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
    ) -> Result<SpineToolCommit, SpineError> {
        let Some(spine_slot) = self.spine.as_ref() else {
            return Ok(Self::no_spine_tool_commit());
        };
        let commit_output =
            validate_grouped_spine_toolcall_outputs(commit_call_id, tool_call_ids, output_items)?;
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
        let completed_toolcall = grouped_completed_toolcall_evidence(
            commit_call_id,
            tool_call_ids,
            request_anchors
                .iter()
                .map(|anchor| (anchor.raw_ordinal, anchor.context_index)),
            &output_raw_ordinals,
            output_context_start,
        )?;
        self.commit_spine_completed_toolcall_with_client_session(
            turn_context,
            client_session,
            commit_call_id,
            commit_output,
            completed_toolcall,
            true,
            false,
        )
        .await
    }

    async fn commit_spine_completed_toolcall_with_client_session(
        self: &Arc<Self>,
        turn_context: &Arc<TurnContext>,
        _client_session: &mut ModelClientSession,
        call_id: &str,
        item: &ResponseItem,
        completed_toolcall: CompletedToolCall,
        tool_resp_already_recorded: bool,
        recorded_output_inside_reduce: bool,
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
        let has_pending_close_commit = matches!(
            pending_commit.as_ref(),
            Some(SpinePendingCommit::Close { .. })
        );
        let current_turn_token_info = self.current_turn_token_usage_info(turn_context).await;
        let pre_compact_token_baselines = if has_pending_close_commit {
            Some(token_baselines_from_info(current_turn_token_info.as_ref()))
        } else {
            None
        };
        let memory_assembly = match pending_commit {
            Some(SpinePendingCommit::Close {
                node,
                suffix_start,
                memory,
                action: _,
                next_summary: _,
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
                let source_plan = {
                    let guard = spine_slot.lock().await;
                    guard.ensure_valid()?;
                    let Some(spine) = guard.runtime() else {
                        return Err(SpineError::Invariant(format!(
                            "spine runtime missing while building close memory source plan for call_id={call_id}"
                        )));
                    };
                    spine.build_close_source_plan(
                        history.raw_items(),
                        &node,
                        suffix_start,
                        toolcall_start,
                        call_id,
                    )
                };
                let source_plan = match source_plan {
                    Ok(source_plan) => source_plan,
                    Err(err) => {
                        let reason = "spine close memory source plan failed before commit";
                        if tool_resp_already_recorded {
                            self.fail_closed_spine_toolcall_commit(call_id, reason)
                                .await;
                        } else {
                            self.abort_spine_pending_tool(call_id, reason).await;
                        }
                        return Err(err);
                    }
                };
                let compact = spine_close_memory_assembly_from_tool_arg(
                    &node.to_string(),
                    &source_plan,
                    &memory,
                )?;
                Some((compact, expected_history))
            }
            Some(SpinePendingCommit::Open) | None => None,
        };
        if let Some((_, expected_history)) = memory_assembly.as_ref() {
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
        let raw_items = self.spine_raw_items_from_rollout_for_commit().await?;
        let mut lock_retries = 0;
        let commit_output = loop {
            let attempt = self.try_commit_spine_tool_output_once(
                spine_slot,
                call_id,
                item,
                memory_assembly.clone(),
                pre_compact_token_baselines,
                current_turn_token_info.as_ref(),
                completed_toolcall.clone(),
                tool_resp_already_recorded,
                &raw_items,
            );
            let attempt = match attempt {
                Ok(attempt) => attempt,
                Err(err) => {
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
            match attempt {
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
            spine_context_already_observed,
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
            record_output: !recorded_output_inside_reduce,
            spine_context_already_observed,
            deferred_tree_update,
        })
    }

    fn try_commit_spine_tool_output_once(
        &self,
        spine_slot: &Mutex<SpineSessionState>,
        call_id: &str,
        tool_resp_item: &ResponseItem,
        memory_assembly: Option<(SpineCloseMemoryAssembly, Vec<ResponseItem>)>,
        pre_compact_token_baselines: Option<SpineTokenBaselines>,
        current_turn_token_info: Option<&TokenUsageInfo>,
        completed_toolcall: CompletedToolCall,
        tool_resp_already_recorded: bool,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<SpineCommitAttempt, SpineError> {
        let Ok(mut guard) = spine_slot.try_lock() else {
            return Ok(SpineCommitAttempt::Retry);
        };
        guard.ensure_valid()?;
        let Some(spine) = guard.runtime_mut() else {
            return Ok(SpineCommitAttempt::RuntimeMissing);
        };
        let Ok(mut state) = self.state.try_lock() else {
            return Ok(SpineCommitAttempt::Retry);
        };
        validate_close_expected_history_for_commit(
            spine,
            call_id,
            memory_assembly.as_ref(),
            state.clone_history().raw_items(),
        )?;
        let memory_assembly = memory_assembly.map(|(compact, _)| compact);
        let pending_commit = spine.pending_commit(call_id)?;
        let token_baselines = token_baselines_for_pending_commit(
            pending_commit.as_ref(),
            pre_compact_token_baselines,
            current_turn_token_info,
        );
        let prepared_commit = prepare_or_observe_completed_toolcall_for_commit(
            spine,
            call_id,
            pending_commit.as_ref(),
            memory_assembly,
            token_baselines,
            completed_toolcall,
            raw_items,
        )?;
        let commit_kind = prepared_commit
            .as_ref()
            .map(|prepared| prepared.kind().clone());
        let defer_tree_update_until_raw_output =
            should_defer_tree_update_until_raw_output(commit_kind.as_ref());
        let mut snapshot = None;
        if let Some(commit_kind) = commit_kind.as_ref() {
            validate_commit_kind_against_history(
                call_id,
                commit_kind,
                state.clone_history().raw_items(),
            )?;
        }
        let history_update = spine_history_update_for_commit_publication(
            spine,
            call_id,
            prepared_commit.as_ref(),
            tool_resp_item,
            tool_resp_already_recorded,
            raw_items,
            state.clone_history().raw_items(),
            state.reference_context_item(),
        )?;
        if let Some(prepared_commit) = prepared_commit.as_ref()
            && let Err(err) = spine.persist_prepared_commit_side_effects(prepared_commit)
        {
            guard.invalidate(format!(
                "failed to persist Spine prepared side effects before publishing h(PS) for call_id={call_id}: {err}"
            ));
            return Err(err);
        }
        if let Some(update) = history_update {
            if let Err(err) =
                Self::apply_spine_history_replacement_to_locked_state(&mut state, update)
            {
                guard.invalidate(format!(
                    "failed to publish Spine h(PS) before installing reduced parse stack for call_id={call_id}: {err}"
                ));
                return Err(SpineError::Invariant(err));
            }
        }
        if let Some(prepared_commit) = prepared_commit {
            spine.install_prepared_commit(prepared_commit);
            let token_info = state.token_info();
            snapshot = Some(build_annotated_tree_snapshot(spine, token_info.as_ref())?);
        }
        Ok(SpineCommitAttempt::Done(SpineCommitOutput {
            snapshot,
            spine_context_already_observed: true,
            defer_tree_update_until_raw_output,
        }))
    }

    async fn spine_raw_items_from_rollout_for_commit(
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
                SpineError::InvalidStore("spine_jit commit requires rollout path".to_string())
            })?;
        let rollout_history = crate::rollout::RolloutRecorder::get_rollout_history(&rollout_path)
            .await
            .map_err(|err| SpineError::InvalidStore(err.to_string()))?;
        Ok(spine_raw_items_after_rollback(
            &rollout_history.get_rollout_items(),
        ))
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
        let Some(spine_slot) = self.spine.as_ref() else {
            return Ok(None);
        };
        let Some(prepared) = self.prepare_spine_root_compact_impl(body).await? else {
            return Ok(None);
        };
        let result = prepared.result().clone();
        let mut guard = spine_slot.lock().await;
        guard.ensure_valid()?;
        let Some(spine) = guard.runtime_mut() else {
            return Ok(None);
        };
        spine.install_prepared_root_compact(prepared);
        let snapshot = spine.build_tree_snapshot()?;
        Ok(Some((result, snapshot)))
    }

    async fn prepare_spine_root_compact_impl(
        &self,
        body: String,
    ) -> Result<Option<crate::spine::SpinePreparedRootCompact>, SpineError> {
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
        let close_baselines = self
            .token_usage_info()
            .await
            .map(|info| SpineTokenBaselines {
                provider_input_tokens: provider_input_context_tokens(&info),
            });
        let token_metadata = SpineRootCompactTokenMetadata {
            close_input_tokens: close_baselines
                .as_ref()
                .and_then(|baselines| baselines.provider_input_tokens),
            close_context_tokens: close_baselines
                .as_ref()
                .and_then(|baselines| baselines.provider_input_tokens),
            next_open_input_tokens: None,
            next_open_context_tokens: None,
        };
        {
            let mut guard = spine_slot.lock().await;
            guard.ensure_valid()?;
            let prepared = match guard
                .runtime_mut()
                .ok_or_else(|| {
                    SpineError::InvalidStore(
                        "spine runtime missing after initialization".to_string(),
                    )
                })?
                .prepare_root_compact_with_checkpoint(
                    &rollout_path,
                    body,
                    &raw_items,
                    token_metadata,
                ) {
                Ok(prepared) => prepared,
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
            Ok(Some(prepared))
        }
    }

    pub(crate) async fn prepare_spine_root_compact_after_native_compact(
        &self,
        items: &mut Vec<ResponseItem>,
        compacted_item: &mut CompactedItem,
    ) -> CodexResult<Option<PreparedSpineRootCompactInstall>> {
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
        let body = spine_root_compact_body(items).ok_or_else(|| {
            CodexErr::SpineTerminalFailure {
                operation: "install Spine root compact".to_string(),
                reason: "native compact replaced host context with no model-visible Spine root memory material"
                    .to_string(),
            }
        })?;
        let Some(root_compact) =
            self.prepare_spine_root_compact_impl(body)
                .await
                .map_err(|err| CodexErr::SpineTerminalFailure {
                    operation: "install Spine root compact".to_string(),
                    reason: err.to_string(),
                })?
        else {
            return Ok(None);
        };
        *items = root_compact.result().materialized.clone();
        compacted_item.replacement_history = Some(items.clone());
        Ok(Some(PreparedSpineRootCompactInstall {
            prepared: root_compact,
        }))
    }

    pub(crate) async fn finalize_spine_root_compact_after_history_publish(
        &self,
        prepared: PreparedSpineRootCompactInstall,
        published_history_len: usize,
    ) -> CodexResult<SpineTreeUpdateEvent> {
        let Some(spine_slot) = self.spine.as_ref() else {
            return Err(CodexErr::SpineTerminalFailure {
                operation: "install Spine root compact".to_string(),
                reason: "spine runtime missing before root compact PS install".to_string(),
            });
        };
        let mut guard = spine_slot.lock().await;
        guard
            .ensure_valid()
            .map_err(|err| CodexErr::SpineTerminalFailure {
                operation: "install Spine root compact".to_string(),
                reason: err.to_string(),
            })?;
        let spine = guard
            .runtime_mut()
            .ok_or_else(|| CodexErr::SpineTerminalFailure {
                operation: "install Spine root compact".to_string(),
                reason: "spine runtime missing before root compact PS install".to_string(),
            })?;
        spine.install_prepared_root_compact(prepared.prepared);
        let current_open_index =
            spine
                .current_open_index()
                .map_err(|err| CodexErr::SpineTerminalFailure {
                    operation: "install Spine root compact".to_string(),
                    reason: err.to_string(),
                })?;
        if current_open_index != published_history_len {
            return Err(CodexErr::SpineTerminalFailure {
                operation: "install Spine root compact".to_string(),
                reason: format!(
                    "spine root compact open index {current_open_index} does not match materialized history length {published_history_len}"
                ),
            });
        }
        spine
            .build_tree_snapshot()
            .map_err(|err| CodexErr::SpineTerminalFailure {
                operation: "install Spine root compact".to_string(),
                reason: err.to_string(),
            })
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

fn spine_root_compact_body(replaced_context: &[ResponseItem]) -> Option<String> {
    let entries = replaced_context
        .iter()
        .enumerate()
        .filter_map(|(index, item)| response_item_visible_text(item).map(|text| (index + 1, text)))
        .collect::<Vec<_>>();
    if entries.is_empty() {
        return None;
    }

    let mut body = "# Spine Native Compact Memory\n\n\
This memory is derived from the host context after native compact succeeded.\n"
        .to_string();
    for (index, text) in entries {
        body.push_str("\n## Replaced Context Item ");
        body.push_str(&index.to_string());
        body.push_str("\n\n");
        body.push_str(text.trim());
        body.push('\n');
    }
    Some(body)
}

fn response_item_visible_text(item: &ResponseItem) -> Option<String> {
    match item {
        ResponseItem::Message { role, content, .. } => {
            let text = content_items_visible_text(content)?;
            Some(format!("{role}: {text}"))
        }
        ResponseItem::Reasoning {
            summary, content, ..
        } => reasoning_visible_text(summary, content.as_deref()),
        ResponseItem::LocalShellCall {
            call_id,
            status,
            action,
            ..
        } => {
            let call_id = call_id.as_deref().unwrap_or("<missing>");
            Some(format!(
                "local_shell_call {call_id} status={status:?}\n{action:?}"
            ))
        }
        ResponseItem::FunctionCall {
            name,
            namespace,
            arguments,
            call_id,
            ..
        } => {
            let tool_name = namespace
                .as_deref()
                .map(|namespace| format!("{namespace}.{name}"))
                .unwrap_or_else(|| name.clone());
            if arguments.trim().is_empty() {
                Some(format!("function_call {call_id}: {tool_name}"))
            } else {
                Some(format!(
                    "function_call {call_id}: {tool_name}\narguments: {arguments}"
                ))
            }
        }
        ResponseItem::ToolSearchCall {
            call_id,
            status,
            execution,
            arguments,
            ..
        } => {
            let call_id = call_id.as_deref().unwrap_or("<missing>");
            let status = status.as_deref().unwrap_or("<unknown>");
            Some(format!(
                "tool_search_call {call_id} status={status} execution={execution}\narguments: {arguments}"
            ))
        }
        ResponseItem::FunctionCallOutput { call_id, output } => {
            function_call_output_visible_text(output)
                .map(|text| format!("function_call_output {call_id}: {text}"))
        }
        ResponseItem::CustomToolCall {
            call_id,
            name,
            input,
            status,
            ..
        } => {
            let status = status.as_deref().unwrap_or("<unknown>");
            if input.trim().is_empty() {
                Some(format!(
                    "custom_tool_call {call_id}: {name} status={status}"
                ))
            } else {
                Some(format!(
                    "custom_tool_call {call_id}: {name} status={status}\ninput: {input}"
                ))
            }
        }
        ResponseItem::CustomToolCallOutput {
            call_id,
            name,
            output,
        } => function_call_output_visible_text(output).map(|text| {
            let name = name.as_deref().unwrap_or("<unknown>");
            format!("custom_tool_call_output {call_id}: {name}: {text}")
        }),
        ResponseItem::ToolSearchOutput {
            call_id,
            status,
            execution,
            tools,
        } => {
            let call_id = call_id.as_deref().unwrap_or("<missing>");
            let tools_text = serde_json::to_string(tools).unwrap_or_else(|_| "[]".to_string());
            Some(format!(
                "tool_search_output {call_id} status={status} execution={execution}\ntools: {tools_text}"
            ))
        }
        ResponseItem::WebSearchCall { status, action, .. } => {
            let status = status.as_deref().unwrap_or("<unknown>");
            Some(format!(
                "web_search_call status={status}\naction: {action:?}"
            ))
        }
        ResponseItem::ImageGenerationCall {
            status,
            revised_prompt,
            ..
        } => {
            let prompt = revised_prompt
                .as_deref()
                .filter(|prompt| !prompt.trim().is_empty())
                .unwrap_or("<none>");
            Some(format!(
                "image_generation_call status={status}\nrevised_prompt: {prompt}"
            ))
        }
        ResponseItem::Compaction { encrypted_content } => {
            non_empty_text(encrypted_content).map(|text| format!("compaction: {text}"))
        }
        ResponseItem::ContextCompaction {
            encrypted_content: Some(encrypted_content),
        } => non_empty_text(encrypted_content).map(|text| format!("context_compaction: {text}")),
        ResponseItem::ContextCompaction {
            encrypted_content: None,
        }
        | ResponseItem::CompactionTrigger
        | ResponseItem::Other => None,
    }
}

fn content_items_visible_text(content: &[ContentItem]) -> Option<String> {
    let text = content
        .iter()
        .filter_map(|item| match item {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                non_empty_text(text)
            }
            ContentItem::InputImage { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    non_empty_text(&text).map(str::to_string)
}

fn reasoning_visible_text(
    summary: &[ReasoningItemReasoningSummary],
    content: Option<&[ReasoningItemContent]>,
) -> Option<String> {
    let mut parts = Vec::new();
    for item in summary {
        let ReasoningItemReasoningSummary::SummaryText { text } = item;
        if let Some(text) = non_empty_text(text) {
            parts.push(format!("reasoning_summary: {text}"));
        }
    }
    if let Some(content) = content {
        for item in content {
            match item {
                ReasoningItemContent::ReasoningText { text }
                | ReasoningItemContent::Text { text } => {
                    if let Some(text) = non_empty_text(text) {
                        parts.push(format!("reasoning_content: {text}"));
                    }
                }
            }
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

fn function_call_output_visible_text(output: &FunctionCallOutputPayload) -> Option<String> {
    output
        .body
        .to_text()
        .and_then(|text| non_empty_text(&text).map(str::to_string))
}

fn non_empty_text(text: &str) -> Option<&str> {
    let text = text.trim();
    (!text.is_empty()).then_some(text)
}

fn build_annotated_tree_snapshot(
    runtime: &SpineRuntime,
    token_info: Option<&TokenUsageInfo>,
) -> Result<SpineTreeUpdateEvent, SpineError> {
    Ok(build_spine_tree_inside_view(runtime, token_info)?.snapshot)
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

fn token_baselines_from_info(current: Option<&TokenUsageInfo>) -> SpineTokenBaselines {
    current
        .map(|current| SpineTokenBaselines {
            provider_input_tokens: provider_input_context_tokens(current),
        })
        .unwrap_or_default()
}

fn token_baselines_for_pending_commit(
    pending_commit: Option<&SpinePendingCommit>,
    pre_compact_token_baselines: Option<SpineTokenBaselines>,
    current_turn_token_info: Option<&TokenUsageInfo>,
) -> SpineTokenBaselines {
    match pending_commit {
        Some(SpinePendingCommit::Close { .. }) => pre_compact_token_baselines
            .unwrap_or_else(|| token_baselines_from_info(current_turn_token_info)),
        Some(SpinePendingCommit::Open) => token_baselines_from_info(current_turn_token_info),
        None => SpineTokenBaselines::default(),
    }
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

fn validate_grouped_spine_toolcall_outputs<'a>(
    commit_call_id: &str,
    tool_call_ids: &[String],
    output_items: &'a [ResponseItem],
) -> Result<&'a ResponseItem, SpineError> {
    let expected_call_ids = tool_call_ids.iter().cloned().collect::<BTreeSet<_>>();
    let mut output_call_ids = BTreeSet::new();
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
    output_items
        .iter()
        .find(|item| tool_response_call_id(item) == Some(commit_call_id))
        .ok_or_else(|| {
            SpineError::InvalidEvent(format!(
                "grouped Spine toolcall missing output for commit call_id={commit_call_id}"
            ))
        })
}

fn completed_toolcall_request_segment(
    raw_ordinal: u64,
    context_index: usize,
) -> CompletedToolCallSegment {
    CompletedToolCallSegment {
        kind: ToolCallSegmentKind::Request,
        raw_ordinal,
        context_index,
    }
}

fn completed_toolcall_response_segment(
    raw_ordinal: u64,
    context_index: usize,
) -> CompletedToolCallSegment {
    CompletedToolCallSegment {
        kind: ToolCallSegmentKind::Response,
        raw_ordinal,
        context_index,
    }
}

fn completed_toolcall_request_segments(
    request_anchors: impl IntoIterator<Item = (u64, usize)>,
) -> Vec<CompletedToolCallSegment> {
    request_anchors
        .into_iter()
        .map(|(raw_ordinal, context_index)| {
            completed_toolcall_request_segment(raw_ordinal, context_index)
        })
        .collect()
}

fn completed_toolcall_response_segments(
    response_raw_ordinals: &[Option<u64>],
    context_start: usize,
) -> Vec<CompletedToolCallSegment> {
    response_raw_ordinals
        .iter()
        .enumerate()
        .filter_map(|(index, raw_ordinal)| {
            raw_ordinal.map(|raw_ordinal| {
                completed_toolcall_response_segment(raw_ordinal, context_start + index)
            })
        })
        .collect()
}

fn grouped_completed_toolcall_evidence(
    commit_call_id: &str,
    tool_call_ids: &[String],
    request_anchors: impl IntoIterator<Item = (u64, usize)>,
    response_raw_ordinals: &[Option<u64>],
    response_context_start: usize,
) -> Result<CompletedToolCall, SpineError> {
    completed_toolcall_evidence(CompletedToolCallEvidenceParts {
        call_id: commit_call_id.to_string(),
        request_call_ids: tool_call_ids.to_vec(),
        request_segments: completed_toolcall_request_segments(request_anchors),
        response_segments: completed_toolcall_response_segments(
            response_raw_ordinals,
            response_context_start,
        ),
        missing_request_error: "completed grouped toolcall must contain at least one request",
        missing_response_error: "completed grouped toolcall must contain at least one response",
    })
}

fn should_defer_tree_update_until_raw_output(commit_kind: Option<&SpineCommitKind>) -> bool {
    matches!(
        commit_kind,
        Some(SpineCommitKind::Close | SpineCommitKind::CloseThenOpen { .. })
    )
}

fn validate_commit_kind_against_history(
    call_id: &str,
    commit_kind: &SpineCommitKind,
    history_items: &[ResponseItem],
) -> Result<(), SpineError> {
    if let SpineCommitKind::Open { open_request_index } = commit_kind
        && *open_request_index > history_items.len()
    {
        return Err(SpineError::Invariant(format!(
            "spine.open request index {open_request_index} exceeds history length {} for call_id={call_id}",
            history_items.len()
        )));
    }
    Ok(())
}

fn validate_close_expected_history_for_commit(
    spine: &mut SpineRuntime,
    call_id: &str,
    memory_assembly: Option<&(SpineCloseMemoryAssembly, Vec<ResponseItem>)>,
    history_items: &[ResponseItem],
) -> Result<(), SpineError> {
    if let Some((_, expected_history)) = memory_assembly
        && history_items != expected_history.as_slice()
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
    Ok(())
}

fn prepare_or_observe_completed_toolcall_for_commit(
    spine: &mut SpineRuntime,
    call_id: &str,
    pending_commit: Option<&SpinePendingCommit>,
    memory_assembly: Option<SpineCloseMemoryAssembly>,
    token_baselines: SpineTokenBaselines,
    completed_toolcall: CompletedToolCall,
    raw_items: &[Option<ResponseItem>],
) -> Result<Option<SpinePreparedCommit>, SpineError> {
    if pending_commit.is_some() {
        spine.prepare_commit_output_with_toolcall_and_raw_items(
            call_id,
            memory_assembly,
            token_baselines,
            completed_toolcall,
            raw_items,
        )
    } else {
        spine.observe_completed_toolcall_with_raw_items(completed_toolcall, raw_items)?;
        Ok(None)
    }
}

fn spine_history_update_for_commit_publication(
    spine: &mut SpineRuntime,
    call_id: &str,
    prepared_commit: Option<&SpinePreparedCommit>,
    tool_resp_item: &ResponseItem,
    tool_resp_already_recorded: bool,
    raw_items: &[Option<ResponseItem>],
    history_items: &[ResponseItem],
    reference_context_item: Option<TurnContextItem>,
) -> Result<Option<SpineHistoryUpdate>, SpineError> {
    if let Some(plan) = prepared_commit.and_then(|prepared| prepared.publication_plan()) {
        return spine_history_update_from_publication_plan(
            call_id,
            plan.operation(),
            plan.suffix_start(),
            plan.replacement_prefix(),
            plan.preserve_host_history_from(),
            plan.append_current_tool_response_if_missing(),
            tool_resp_item,
            tool_resp_already_recorded,
            history_items,
            reference_context_item,
        )
        .map(Some);
    }
    if tool_resp_already_recorded {
        return Ok(spine_history_update_from_materialized_projection(
            call_id,
            history_items,
            spine.materialize_history(raw_items)?,
            reference_context_item,
        ));
    }
    Ok(None)
}

fn spine_history_update_from_publication_plan(
    call_id: &str,
    operation: &'static str,
    suffix_start: usize,
    replacement_prefix: &[ResponseItem],
    preserve_host_history_from: usize,
    append_current_tool_response_if_missing: bool,
    tool_resp_item: &ResponseItem,
    tool_resp_already_recorded: bool,
    history_items: &[ResponseItem],
    reference_context_item: Option<TurnContextItem>,
) -> Result<SpineHistoryUpdate, SpineError> {
    let suffix_end = history_items.len();
    if suffix_start > suffix_end {
        return Err(SpineError::Invariant(format!(
            "{operation} suffix start {suffix_start} exceeds history length {suffix_end} for call_id={call_id}"
        )));
    }
    if preserve_host_history_from > suffix_end {
        return Err(SpineError::Invariant(format!(
            "{operation} preserve-host-history index {preserve_host_history_from} exceeds history length {suffix_end} for call_id={call_id}"
        )));
    }
    let mut replacement = replacement_prefix.to_vec();
    replacement.extend_from_slice(&history_items[preserve_host_history_from..]);
    if append_current_tool_response_if_missing && !tool_resp_already_recorded {
        replacement.push(tool_resp_item.clone());
    }
    Ok(SpineHistoryUpdate {
        call_id: call_id.to_string(),
        operation,
        suffix_start,
        expected_history: history_items.to_vec(),
        replacement,
        reference_context_item,
    })
}

fn spine_history_update_from_materialized_projection(
    call_id: &str,
    history_items: &[ResponseItem],
    materialized: Vec<ResponseItem>,
    reference_context_item: Option<TurnContextItem>,
) -> Option<SpineHistoryUpdate> {
    if materialized.as_slice() == history_items {
        return None;
    }
    Some(SpineHistoryUpdate {
        call_id: call_id.to_string(),
        operation: "spine toolcall projection",
        suffix_start: 0,
        expected_history: history_items.to_vec(),
        replacement: materialized,
        reference_context_item,
    })
}

fn completed_toolcall_evidence(
    parts: CompletedToolCallEvidenceParts,
) -> Result<CompletedToolCall, SpineError> {
    let CompletedToolCallEvidenceParts {
        call_id,
        request_call_ids,
        mut request_segments,
        mut response_segments,
        missing_request_error,
        missing_response_error,
    } = parts;
    request_segments.sort_by_key(|segment| (segment.context_index, segment.raw_ordinal));
    response_segments.sort_by_key(|segment| (segment.context_index, segment.raw_ordinal));
    if request_segments.is_empty() {
        return Err(SpineError::InvalidEvent(missing_request_error.to_string()));
    }
    if response_segments.is_empty() {
        return Err(SpineError::InvalidEvent(missing_response_error.to_string()));
    }
    let mut segments = Vec::with_capacity(request_segments.len() + response_segments.len());
    segments.extend(request_segments);
    segments.extend(response_segments);
    Ok(CompletedToolCall {
        call_id,
        request_call_ids,
        segments,
    })
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
