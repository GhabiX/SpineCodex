use super::*;
use crate::client::ModelClientSession;
use crate::context_manager::ContextAppend;
use crate::context_manager::ContextManager;
use crate::context_manager::estimate_response_item_model_visible_bytes;
use crate::session::rollout_reconstruction::ReplacementHistoryBoundary;
use crate::session::spine_compact::SpineCloseCompactOutcome;
use crate::session::spine_tree_inside::build_spine_tree_inside_view;
use crate::spine::SPINE_NAMESPACE;
use crate::spine::SpineCloneBoundary;
use crate::spine::SpineCloseCompact;
use crate::spine::SpineCommitKind;
use crate::spine::SpinePendingCommit;
use crate::spine::SpineRootCompactResult;
use crate::spine::SpineRootCompactTokenMetadata;
use crate::spine::SpineRuntime;
use crate::spine::SpineStore;
use crate::spine::SpineTokenBaselines;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::TokenUsageInfo;
use codex_protocol::spine_tree::SpineTreeUpdateEvent;
use codex_rollout::should_persist_response_item;
use codex_utils_output_truncation::approx_tokens_from_byte_count_i64;

pub(super) struct PreparedSpineReplay {
    raw_len: u64,
    runtime: Option<SpineRuntime>,
    materialized: Option<Vec<ResponseItem>>,
}

#[derive(Debug)]
pub(crate) struct SpineToolCommit {
    pub(crate) output_text: Option<String>,
    pub(crate) record_output: bool,
}

type SpineCommitOutput = (Option<SpineTreeUpdateEvent>, Option<String>);
const SPINE_COMMIT_LOCK_RETRY_LIMIT: usize = 4096;

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
        }
    }

    fn skip_spine_tool_output_commit() -> SpineToolCommit {
        SpineToolCommit {
            output_text: None,
            record_output: false,
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
        if !used_replacement_history
            && let Some(runtime) = runtime.as_ref()
            && runtime.has_live_root_compact_event()?
        {
            return Err(SpineError::InvalidStore(
                "spine_jit root compact sidecar is missing rollout compact boundary".to_string(),
            ));
        }
        let materialized = runtime
            .as_ref()
            .map(|runtime| runtime.materialize_history(raw_items))
            .transpose()?;
        if used_replacement_history {
            let store = SpineStore::for_rollout(&rollout_path)?;
            let raw_live = raw_items.iter().map(Option::is_some).collect::<Vec<_>>();
            for boundary in replacement_history_boundaries {
                store.validate_compact_checkpoint_for_boundary(
                    &rollout_path,
                    &raw_live,
                    boundary.raw_boundary,
                    &boundary.replacement_history,
                )?;
            }
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
        let history = self.clone_history().await;
        let raw_history = history.raw_items().to_vec();
        let mut guard = spine_slot.lock().await;
        guard.ensure_valid()?;
        let Some(runtime) = guard.runtime_mut() else {
            return Ok(());
        };
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
                let context = raw_history
                    .get(..append.context_index)
                    .ok_or_else(|| {
                        SpineError::InvalidEvent(
                            "checkpoint context index outside history".to_string(),
                        )
                    })?
                    .to_vec();
                runtime.checkpoint_before_user_msg(&rollout_path, raw_ordinal, &context)?;
            }
            runtime.observe_context_item(raw_ordinal, append.context_index, item)?;
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
        let ResponseItem::FunctionCallOutput { call_id, .. } = item else {
            return Ok(Self::no_spine_tool_commit());
        };
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
        let close_compact = match pending_commit {
            Some(SpinePendingCommit::Close {
                node,
                suffix_start,
                instruction,
            }) => {
                let history = self.clone_history().await;
                let expected_history = history.raw_items().to_vec();
                let compact = match Box::pin(self.spine_compact_close(
                    turn_context,
                    client_session,
                    &history,
                    node.to_string(),
                    suffix_start,
                    item,
                    instruction,
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
                        self.abort_spine_pending_tool(
                            call_id,
                            "spine close invalidated by native compact",
                        )
                        .await;
                        return Ok(Self::skip_spine_tool_output_commit());
                    }
                    Err(err) => {
                        self.abort_spine_pending_tool(
                            call_id,
                            "spine close compact failed before commit",
                        )
                        .await;
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
                self.abort_spine_pending_tool(call_id, "spine close history changed before commit")
                    .await;
                return Err(SpineError::Operation(format!(
                    "spine.close history changed while compacting suffix for call_id={call_id}"
                )));
            }
        }
        let mut lock_retries = 0;
        let (snapshot, output_text) = loop {
            match self.try_commit_spine_tool_output_once(
                spine_slot,
                call_id,
                close_compact.clone(),
            )? {
                SpineCommitAttempt::Done(output) => break output,
                SpineCommitAttempt::RuntimeMissing => {
                    return Ok(Self::no_spine_tool_commit());
                }
                SpineCommitAttempt::Retry if lock_retries < SPINE_COMMIT_LOCK_RETRY_LIMIT => {
                    lock_retries += 1;
                    tokio::task::yield_now().await;
                }
                SpineCommitAttempt::Retry => {
                    self.abort_spine_pending_tool(
                        call_id,
                        "spine tool output commit lock retry limit exceeded before commit",
                    )
                    .await;
                    return Err(SpineError::Operation(format!(
                        "spine tool output commit could not acquire session locks after {SPINE_COMMIT_LOCK_RETRY_LIMIT} retries for call_id={call_id}"
                    )));
                }
            }
        };
        if let Some(snapshot) = snapshot {
            self.send_spine_tree_update(turn_context, snapshot).await;
        }
        Ok(SpineToolCommit {
            output_text,
            record_output: true,
        })
    }

    fn try_commit_spine_tool_output_once(
        &self,
        spine_slot: &Mutex<SpineSessionState>,
        call_id: &str,
        close_compact: Option<(SpineCloseCompact, Vec<ResponseItem>)>,
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
        let token_baselines = token_baselines_from_info(state.token_info().as_ref());
        let commit_kind = spine.maybe_commit_output_with_token_baselines(
            call_id,
            close_compact,
            token_baselines,
        )?;
        let mut snapshot = None;
        let mut output_text = None;
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
                    let filtered = history_items
                        .iter()
                        .cloned()
                        .enumerate()
                        .filter_map(|(index, history_item)| {
                            let remove = index >= *open_request_index
                                && match &history_item {
                                    ResponseItem::FunctionCall {
                                        call_id: existing,
                                        namespace,
                                        ..
                                    } => {
                                        existing == call_id
                                            && namespace.as_deref() == Some(SPINE_NAMESPACE)
                                    }
                                    ResponseItem::FunctionCallOutput {
                                        call_id: existing, ..
                                    } => existing == call_id,
                                    _ => false,
                                };
                            (!remove).then_some(history_item)
                        })
                        .collect();
                    let reference_context_item = state.reference_context_item();
                    state.replace_history(filtered, reference_context_item);
                }
                SpineCommitKind::Close {
                    suffix_start,
                    replacement,
                } => {
                    let suffix_end = state.clone_history().raw_items().len();
                    if *suffix_start > suffix_end {
                        return Err(SpineError::Invariant(format!(
                            "spine.close suffix start {suffix_start} exceeds history length {suffix_end} for call_id={call_id}"
                        )));
                    }
                    let reference_context_item = state.reference_context_item();
                    state
                        .replace_history_suffix(
                            *suffix_start..suffix_end,
                            replacement.clone(),
                            reference_context_item,
                        )
                        .map_err(SpineError::Invariant)?;
                }
                SpineCommitKind::CloseThenOpen {
                    suffix_start,
                    replacement,
                    ..
                } => {
                    let suffix_end = state.clone_history().raw_items().len();
                    if *suffix_start > suffix_end {
                        return Err(SpineError::Invariant(format!(
                            "spine.next suffix start {suffix_start} exceeds history length {suffix_end} for call_id={call_id}"
                        )));
                    }
                    let reference_context_item = state.reference_context_item();
                    state
                        .replace_history_suffix(
                            *suffix_start..suffix_end,
                            replacement.clone(),
                            reference_context_item,
                        )
                        .map_err(SpineError::Invariant)?;
                }
            }
            let token_info = state.token_info();
            snapshot = Some(build_annotated_tree_snapshot(spine, token_info.as_ref())?);
            let tree = render_spine_tree_for_model(spine, token_info)?;
            output_text = Some(format!("{output_prefix}\n\n{tree}"));
        }
        Ok(SpineCommitAttempt::Done((snapshot, output_text)))
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

    pub(crate) async fn install_spine_root_compact(
        &self,
        body: String,
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
            next_open_input_tokens: None,
            next_open_context_tokens: None,
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
    ) -> CodexResult<Option<SpineTreeUpdateEvent>> {
        let Some(spine_slot) = self.spine.as_ref() else {
            return Ok(None);
        };
        {
            let guard = spine_slot.lock().await;
            guard.ensure_valid().map_err(|err| {
                CodexErr::SpineTerminalFailure {
                    operation: "install Spine root compact".to_string(),
                    reason: err.to_string(),
                }
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
        let Some((root_compact, snapshot)) =
            self.install_spine_root_compact(body).await.map_err(|err| {
                CodexErr::SpineTerminalFailure {
                    operation: "install Spine root compact".to_string(),
                    reason: err.to_string(),
                }
            })?
        else {
            return Ok(None);
        };
        *items = root_compact.materialized;
        compacted_item.replacement_history = Some(items.clone());
        Ok(Some(snapshot))
    }
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
