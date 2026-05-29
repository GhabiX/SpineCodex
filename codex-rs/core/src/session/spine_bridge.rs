use super::*;
use crate::context_manager::ContextAppend;
use crate::context_manager::ContextManager;
use crate::context_manager::estimate_response_item_model_visible_bytes;
use crate::session::rollout_reconstruction::ReplacementHistoryBoundary;
use crate::spine::SPINE_NAMESPACE;
use crate::spine::SpineCommitKind;
use crate::spine::SpinePendingCommit;
use crate::spine::SpineRootCompactResult;
use crate::spine::SpineRuntime;
use crate::spine::SpineTokenBaselines;
use codex_protocol::num_format::format_si_suffix;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::TokenUsageInfo;
use codex_protocol::spine_tree::SpineNodeContextUnavailableReason;
use codex_protocol::spine_tree::SpineTreeNodeAccountingSnapshot;
use codex_protocol::spine_tree::SpineTreeUpdateEvent;
use codex_rollout::should_persist_response_item;
use codex_utils_output_truncation::approx_tokens_from_byte_count_i64;

pub(super) struct PreparedSpineReplay {
    raw_len: u64,
    runtime: Option<SpineRuntime>,
    materialized: Option<Vec<ResponseItem>>,
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
            let mut snapshot = runtime.build_tree_snapshot()?;
            annotate_current_node_context(
                &mut snapshot,
                token_info.as_ref(),
                runtime.current_open_context_tokens(),
                runtime.current_open_context_baseline_source(),
            );
            snapshot
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
                "spine_task_tree resume requires Spine sidecar".to_string(),
            ));
        }
        if !used_replacement_history
            && let Some(runtime) = runtime.as_ref()
            && runtime.has_live_root_compact_event()?
        {
            return Err(SpineError::InvalidStore(
                "spine_task_tree root compact sidecar is missing rollout compact boundary"
                    .to_string(),
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
                SpineError::InvalidStore(
                    "spine_task_tree checkpoint requires rollout path".to_string(),
                )
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
                "spine_task_tree is disabled or this session has no persisted rollout".to_string(),
            ));
        };
        let Some(rollout_path) = self
            .current_rollout_path()
            .await
            .map_err(|err| SpineError::InvalidStore(err.to_string()))?
        else {
            return Err(SpineError::InvalidStore(
                "spine_task_tree requires a persisted rollout".to_string(),
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
        let current_annotation =
            format_current_node_context(token_info.as_ref(), runtime.current_open_context_tokens());
        let mut tree = if let Some(annotation) = current_annotation.as_deref() {
            runtime.render_tree_with_current_annotation(Some(annotation))?
        } else {
            runtime.render_tree()?
        };
        if let Some(line) = format_context_window_pressure(token_info) {
            tree.push_str("\n\n");
            tree.push_str(&line);
        }
        Ok(tree)
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
            let mut snapshot = runtime.build_tree_snapshot()?;
            annotate_current_node_context(
                &mut snapshot,
                token_info.as_ref(),
                runtime.current_open_context_tokens(),
                runtime.current_open_context_baseline_source(),
            );
            snapshot
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

    pub(crate) async fn maybe_commit_spine_tool_output(
        &self,
        turn_context: &TurnContext,
        item: &ResponseItem,
    ) -> Result<(), SpineError> {
        let ResponseItem::FunctionCallOutput { call_id, .. } = item else {
            return Ok(());
        };
        let Some(spine_slot) = self.spine.as_ref() else {
            return Ok(());
        };
        let pending_commit = {
            let guard = spine_slot.lock().await;
            guard.ensure_valid()?;
            let Some(spine) = guard.runtime() else {
                return Ok(());
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
                Some((
                    self.spine_compact_close(
                        turn_context,
                        &history,
                        node.to_string(),
                        suffix_start,
                        item,
                        instruction,
                    )
                    .await?,
                    expected_history,
                ))
            }
            Some(SpinePendingCommit::Open) | None => None,
        };
        if let Some((_, expected_history)) = close_compact.as_ref() {
            let history = self.clone_history().await;
            if history.raw_items() != expected_history.as_slice() {
                return Err(SpineError::InvalidEvent(
                    "spine.close history changed while compacting suffix".to_string(),
                ));
            }
        }
        let snapshot = {
            let mut guard = spine_slot.lock().await;
            guard.ensure_valid()?;
            let Some(spine) = guard.runtime_mut() else {
                return Ok(());
            };
            let mut state = self.state.lock().await;
            if let Some((_, expected_history)) = close_compact.as_ref()
                && state.clone_history().raw_items() != expected_history.as_slice()
            {
                return Err(SpineError::InvalidEvent(
                    "spine.close history changed before suffix replacement".to_string(),
                ));
            }
            let close_compact = close_compact.map(|(compact, _)| compact);
            let token_baselines = token_baselines_from_info(state.token_info().as_ref());
            let commit_kind = spine.maybe_commit_output_with_token_baselines(
                call_id,
                close_compact,
                token_baselines,
            )?;
            let mut snapshot = None;
            if let Some(commit_kind) = commit_kind.as_ref() {
                match commit_kind {
                    SpineCommitKind::Open { open_request_index } => {
                        let history = state.clone_history();
                        let history_items = history.raw_items();
                        if *open_request_index > history_items.len() {
                            return Err(SpineError::InvalidEvent(format!(
                                "spine.open request index {open_request_index} exceeds history length {}",
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
                                            call_id: existing,
                                            ..
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
                            return Err(SpineError::InvalidEvent(format!(
                                "spine.close suffix start {suffix_start} exceeds history length {suffix_end}"
                            )));
                        }
                        let reference_context_item = state.reference_context_item();
                        state
                            .replace_history_suffix(
                                *suffix_start..suffix_end,
                                replacement.clone(),
                                reference_context_item,
                            )
                            .map_err(SpineError::InvalidEvent)?;
                    }
                }
                let mut tree_snapshot = spine.build_tree_snapshot()?;
                let token_info = state.token_info();
                annotate_current_node_context(
                    &mut tree_snapshot,
                    token_info.as_ref(),
                    spine.current_open_context_tokens(),
                    spine.current_open_context_baseline_source(),
                );
                snapshot = Some(tree_snapshot);
            }
            snapshot
        };
        if let Some(snapshot) = snapshot {
            self.send_spine_tree_update(turn_context, snapshot).await;
        }
        Ok(())
    }

    pub(crate) async fn is_pending_spine_close_output(
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
                SpineError::InvalidStore(
                    "spine_task_tree root compact requires rollout path".to_string(),
                )
            })?;
        let history = crate::rollout::RolloutRecorder::get_rollout_history(&rollout_path)
            .await
            .map_err(|err| SpineError::InvalidStore(err.to_string()))?;
        let raw_items = spine_raw_items_after_rollback(&history.get_rollout_items());
        let next_open_baselines = token_baselines_from_info(self.token_usage_info().await.as_ref());
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
                .root_compact_with_checkpoint(
                    &rollout_path,
                    body,
                    &raw_items,
                    next_open_baselines.input_tokens,
                    next_open_baselines.context_tokens,
                ) {
                Ok(result) => result,
                Err(err) => {
                    guard.invalidate(format!("failed to install Spine root compact: {err}"));
                    return Err(err);
                }
            };
            let Some(spine) = guard.runtime() else {
                return Ok(None);
            };
            let current_open_index = spine.current_open_index()?;
            if current_open_index != result.materialized.len() {
                return Err(SpineError::InvalidEvent(format!(
                    "spine root compact open index {current_open_index} does not match materialized history length {}",
                    result.materialized.len()
                )));
            }
            let snapshot = spine.build_tree_snapshot()?;
            return Ok(Some((result, snapshot)));
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
                CodexErr::Fatal(format!("failed to install Spine root compact: {err}"))
            })?;
            if guard.runtime().is_none() {
                return Ok(None);
            }
        }
        let body = spine_root_compact_body(items, compacted_item).ok_or_else(|| {
            CodexErr::Fatal(
                "native compact produced no model-visible Spine root memory body".to_string(),
            )
        })?;
        let Some((root_compact, snapshot)) =
            self.install_spine_root_compact(body).await.map_err(|err| {
                CodexErr::Fatal(format!("failed to install Spine root compact: {err}"))
            })?
        else {
            return Ok(None);
        };
        *items = root_compact.materialized.clone();
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

fn format_current_node_context(
    current: Option<&TokenUsageInfo>,
    open_context_tokens: Option<i64>,
) -> Option<String> {
    let tokens = current_node_context_tokens(current, open_context_tokens).ok()??;
    Some(format!("(~{} node context)", format_si_suffix(tokens)))
}

fn annotate_current_node_context(
    snapshot: &mut SpineTreeUpdateEvent,
    current: Option<&TokenUsageInfo>,
    open_context_tokens: Option<i64>,
    open_context_source: Option<codex_protocol::spine_tree::SpineNodeContextBaselineSource>,
) {
    let active_node_id = snapshot.active_node_id.as_str();
    let Some(active_node) = snapshot
        .nodes
        .iter_mut()
        .find(|node| node.node_id == active_node_id)
    else {
        return;
    };
    let accounting = active_node
        .accounting
        .get_or_insert_with(SpineTreeNodeAccountingSnapshot::default);
    match current_node_context_tokens(current, open_context_tokens) {
        Ok(Some(tokens)) => {
            accounting.current_node_context_tokens = Some(tokens);
            accounting.current_node_context_baseline_source = open_context_source;
            accounting.current_node_context_unavailable = None;
        }
        Ok(None) => {
            accounting.current_node_context_tokens = None;
            accounting.current_node_context_baseline_source = open_context_source;
            accounting.current_node_context_unavailable =
                Some(SpineNodeContextUnavailableReason::NonPositiveDelta);
        }
        Err(reason) => {
            accounting.current_node_context_tokens = None;
            accounting.current_node_context_baseline_source = open_context_source;
            accounting.current_node_context_unavailable = Some(reason);
        }
    }
}

fn current_node_context_tokens(
    current: Option<&TokenUsageInfo>,
    open_context_tokens: Option<i64>,
) -> Result<Option<i64>, SpineNodeContextUnavailableReason> {
    let current = current
        .ok_or(SpineNodeContextUnavailableReason::MissingCurrentUsage)?
        .last_token_usage
        .tokens_in_context_window();
    let open_context_tokens =
        open_context_tokens.ok_or(SpineNodeContextUnavailableReason::MissingOpenContextBaseline)?;
    let tokens = current.saturating_sub(open_context_tokens);
    Ok((tokens > 0).then_some(tokens))
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

fn format_context_window_pressure(info: Option<TokenUsageInfo>) -> Option<String> {
    let info = info?;
    let window = info.model_context_window?;
    if window <= 0 {
        return None;
    }
    let usage = info.last_token_usage;
    let used = usage.tokens_in_context_window();
    if used <= 0 {
        return None;
    }
    let remaining = usage
        .percent_of_context_window_remaining(window)
        .clamp(0, 100);
    Some(format!(
        "Context window: {remaining}% left ({} used / {})",
        format_si_suffix(used),
        format_si_suffix(window)
    ))
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
