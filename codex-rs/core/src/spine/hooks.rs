use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::TurnContextItem;
use codex_protocol::spine_tree::SpineTreeUpdateEvent;
use std::future::Future;
use std::path::Path;

#[cfg(test)]
use super::runtime::IntoSpineNodeMemory;
use super::runtime::SpineError;
use super::runtime::SpineSessionState;
use super::store::SpineCloneBoundary;

pub(crate) struct HostEffects {
    inner: super::runtime::SpineHostEffects,
}

pub(crate) struct TreeHostUpdates {
    inner: super::runtime::SpineTreeHostUpdates,
}

pub(crate) struct HistoryHostEffect {
    inner: super::runtime::SpineHostEffect,
}

pub(crate) struct CompletedToolCallHostOutcome {
    inner: super::runtime::SpineCompletedToolCallHostOutcome,
}

pub(crate) struct ToolcallHostCommitAttempt {
    inner: super::runtime::SpineToolcallHostCommitAttempt,
}

pub(crate) struct ToolcallHostAttempt {
    inner: super::runtime::SpineToolcallHostAttempt,
}

pub(crate) struct SingleToolcallOutputRecordingPlan {
    inner: super::runtime::SpineSingleToolcallOutputRecordingPlan,
}

pub(crate) struct GroupedToolcallOutputRecordingPlan {
    inner: super::runtime::SpineGroupedToolcallOutputRecordingPlan,
}

pub(crate) enum ToolcallOutputRecordingRequest<'a> {
    Single {
        call_id: &'a str,
        raw_items: &'a [Option<ResponseItem>],
    },
    Grouped {
        output_items: &'a [ResponseItem],
    },
}

pub(crate) enum ToolcallOutputRecordingPlan {
    Single(Option<SingleToolcallOutputRecordingPlan>),
    Grouped(GroupedToolcallOutputRecordingPlan),
}

pub(crate) struct ReplayRuntime {
    inner: super::runtime::PreparedSpineReplayRuntime,
}

pub(crate) struct InitEvidence<'a> {
    pub(crate) rollout_path: &'a Path,
}

pub(crate) struct CompactEvidence<'a> {
    pub(crate) rollout_path: &'a Path,
    pub(crate) compacted_history: &'a [ResponseItem],
    pub(crate) raw_items: &'a [Option<ResponseItem>],
    pub(crate) close_provider_input_tokens: Option<i64>,
}

pub(crate) struct NativeCompactEvidence<'a> {
    pub(crate) compacted_history: &'a [ResponseItem],
    pub(crate) native_items: &'a [ResponseItem],
}

#[derive(Clone, Debug)]
pub(crate) struct MessageEvidence<'a> {
    pub(crate) rollout_path: &'a Path,
    pub(crate) raw_ordinal: u64,
    pub(crate) context_index: usize,
    pub(crate) item: &'a ResponseItem,
    pub(crate) raw_items: &'a [Option<ResponseItem>],
}

pub(crate) struct ToolCallEvidence<'a> {
    inner: super::runtime::SpineToolCallEvidence<'a>,
}

pub(crate) struct ToolcallHookEvidence<'a> {
    pub(crate) completed_output: &'a CompletedToolCallOutputEvidence<'a>,
    pub(crate) output_raw_ordinals: &'a [Option<u64>],
    pub(crate) output_context_start: usize,
    pub(crate) raw_items: &'a [Option<ResponseItem>],
    pub(crate) current_turn_provider_input_tokens: Option<i64>,
    pub(crate) tool_resp_already_recorded: bool,
    pub(crate) recorded_inside_reduce: bool,
}

pub(crate) struct CompletedToolCallOutputEvidence<'a> {
    inner: super::runtime::SpineCompletedToolCallOutputEvidence<'a>,
}

#[derive(Clone, Debug)]
pub(crate) struct ObservedContextItem<'a> {
    pub(crate) raw_ordinal: u64,
    pub(crate) context_index: usize,
    pub(crate) item: &'a ResponseItem,
}

pub(crate) fn new_session_state(jit_enabled: bool, trim_enabled: bool) -> SpineSessionState {
    SpineSessionState::new_with_features(jit_enabled, trim_enabled)
}

pub(crate) fn ensure_runtime(
    state: &mut SpineSessionState,
    rollout_path: &Path,
) -> Result<(), SpineError> {
    state.ensure_runtime(rollout_path)
}

pub(crate) fn observe_raw_items(
    state: &mut SpineSessionState,
    count: usize,
) -> Result<(), SpineError> {
    state.observe_raw_items(count)
}

pub(crate) fn take_initial_tree_snapshot(
    state: &mut SpineSessionState,
) -> Result<Option<SpineTreeUpdateEvent>, SpineError> {
    state.take_initial_tree_snapshot()
}

pub(crate) fn tree_snapshot_projection(
    state: &SpineSessionState,
) -> Result<
    Option<(
        SpineTreeUpdateEvent,
        Vec<super::runtime::SpineOpenNodeContextProjection>,
    )>,
    SpineError,
> {
    state.tree_snapshot_projection()
}

pub(crate) fn render_tree_with_context_annotations(
    state: &SpineSessionState,
    annotations: &std::collections::BTreeMap<super::model::NodeId, String>,
) -> Result<Option<String>, SpineError> {
    state.render_tree_with_context_annotations(annotations)
}

pub(crate) fn trim_projection_needs_rollout_raw_items(
    state: &SpineSessionState,
) -> Result<Option<bool>, SpineError> {
    state.trim_projection_needs_rollout_raw_items()
}

pub(crate) fn materialize_trim_projection_from_raw_items(
    state: &SpineSessionState,
    raw_items: &[Option<ResponseItem>],
) -> Result<Option<Vec<ResponseItem>>, SpineError> {
    state.materialize_trim_projection_from_raw_items(raw_items)
}

pub(crate) fn project_trim_projection_from_history(
    state: &SpineSessionState,
    history_items: &[ResponseItem],
) -> Result<Option<Vec<ResponseItem>>, SpineError> {
    state.project_trim_projection_from_history(history_items)
}

pub(crate) fn current_trim_targets_for_prompt(
    state: &SpineSessionState,
    raw_items: &[Option<ResponseItem>],
) -> Result<Option<Vec<super::runtime::SpineCurrentTrimTarget>>, SpineError> {
    state.current_trim_targets_for_prompt(raw_items)
}

pub(crate) fn is_ready(state: &SpineSessionState) -> bool {
    state.is_ready()
}

pub(crate) fn ensure_observable_context(state: &SpineSessionState) -> Result<(), SpineError> {
    state.ensure_observable_context()
}

#[cfg(test)]
pub(crate) fn is_ready_for_test_root_compact(
    state: &SpineSessionState,
) -> Result<bool, SpineError> {
    state.ensure_valid()?;
    Ok(state.is_ready())
}

#[cfg(test)]
pub(crate) fn install_test_root_compact_after_history_publish(
    state: &mut SpineSessionState,
    prepared: super::runtime::SpineRootCompactHostInstall,
    published_history_len: usize,
) -> Result<SpineTreeUpdateEvent, SpineError> {
    state.apply_root_compact_after_history_publish(prepared, published_history_len)
}

#[cfg(test)]
pub(crate) fn prepare_test_root_compact_apply_with_checkpoint(
    state: &mut SpineSessionState,
    rollout_path: &Path,
    body: String,
    raw_items: &[Option<ResponseItem>],
    close_provider_input_tokens: Option<i64>,
) -> Result<super::runtime::SpineRootCompactHostInstall, SpineError> {
    state.prepare_native_root_compact_apply_with_checkpoint(
        rollout_path,
        body,
        raw_items,
        close_provider_input_tokens,
    )
}

pub(crate) fn invalidate_runtime(state: &mut SpineSessionState, reason: String) {
    state.invalidate(reason);
}

pub(crate) fn release_runtime_for_shutdown(state: &mut SpineSessionState) {
    state.release_runtime_for_shutdown();
}

pub(crate) fn release_runtime_for_replay(state: &mut SpineSessionState) {
    state.release_runtime_for_replay();
}

pub(crate) fn install_cloned_sidecar_for_fork(
    state: &mut SpineSessionState,
    boundary: &SpineCloneBoundary,
    target_rollout_path: &Path,
    raw_items: &[Option<ResponseItem>],
) -> Result<(), SpineError> {
    state.install_cloned_sidecar_for_fork(boundary, target_rollout_path, raw_items)
}

pub(crate) fn abort_pending_tool(
    state: &mut SpineSessionState,
    call_id: &str,
) -> Result<bool, SpineError> {
    state.abort_pending_tool(call_id)
}

pub(crate) fn abort_any_pending(
    state: &mut SpineSessionState,
) -> Result<Option<String>, SpineError> {
    state.abort_any_pending()
}

pub(crate) fn is_control_output_call_id(
    state: &SpineSessionState,
    call_id: &str,
) -> Result<bool, SpineError> {
    state.is_control_output_call_id(call_id)
}

#[cfg(test)]
pub(crate) fn test_seed_open_control_request(
    state: &mut SpineSessionState,
    call_id: String,
    summary: String,
) -> Result<(), SpineError> {
    state.test_seed_open_control_request(call_id, summary)
}

#[cfg(test)]
pub(crate) fn test_seed_close_control_request<M: IntoSpineNodeMemory>(
    state: &mut SpineSessionState,
    call_id: String,
    memory: M,
) -> Result<(), SpineError> {
    state.test_seed_close_control_request(call_id, memory)
}

#[cfg(test)]
pub(crate) fn test_seed_next_control_request<M: IntoSpineNodeMemory>(
    state: &mut SpineSessionState,
    call_id: String,
    summary: String,
    memory: M,
) -> Result<(), SpineError> {
    state.test_seed_next_control_request(call_id, summary, memory)
}

pub(crate) fn observe_provider_token_usage(
    state: &mut SpineSessionState,
    input_tokens: Option<i64>,
) {
    state.observe_provider_token_usage(input_tokens);
}

impl<'a> ToolCallEvidence<'a> {
    pub(crate) fn single(item: &'a ResponseItem) -> Self {
        Self {
            inner: super::runtime::SpineToolCallEvidence::single(item),
        }
    }

    pub(crate) fn grouped(
        commit_call_id: &'a str,
        tool_call_ids: &'a [String],
        output_items: &'a [ResponseItem],
    ) -> Self {
        Self {
            inner: super::runtime::SpineToolCallEvidence::grouped(
                commit_call_id,
                tool_call_ids,
                output_items,
            ),
        }
    }

    pub(crate) fn grouped_as_ordinary(
        commit_call_id: &'a str,
        tool_call_ids: &'a [String],
        output_items: &'a [ResponseItem],
    ) -> Self {
        Self {
            inner: super::runtime::SpineToolCallEvidence::grouped_as_ordinary(
                commit_call_id,
                tool_call_ids,
                output_items,
            ),
        }
    }

    pub(crate) fn completed_output(
        &self,
    ) -> Result<Option<CompletedToolCallOutputEvidence<'a>>, SpineError> {
        self.inner
            .completed_output()
            .map(|output| output.map(CompletedToolCallOutputEvidence::from_runtime))
    }
}

impl HostEffects {
    pub(crate) fn none() -> Self {
        Self::from_runtime(super::runtime::SpineHostEffects::none())
    }

    pub(crate) fn from_runtime(inner: super::runtime::SpineHostEffects) -> Self {
        Self { inner }
    }

    pub(crate) fn extend(&mut self, effects: Self) {
        self.inner.extend(effects.inner);
    }

    pub(crate) async fn apply_after_batch_materialized_history_request<
        E,
        ApplyEffects,
        ApplyEffectsFuture,
        PublishMaterializedHistory,
        PublishMaterializedHistoryFuture,
    >(
        self,
        apply_effects: ApplyEffects,
        publish_materialized_history: PublishMaterializedHistory,
    ) -> Result<(), E>
    where
        ApplyEffects: FnOnce(Self) -> ApplyEffectsFuture,
        ApplyEffectsFuture: Future<Output = Result<(), E>>,
        PublishMaterializedHistory: FnOnce() -> PublishMaterializedHistoryFuture,
        PublishMaterializedHistoryFuture: Future<Output = Result<(), E>>,
    {
        self.inner
            .apply_after_batch_materialized_history_request(
                |effects| apply_effects(Self::from_runtime(effects)),
                publish_materialized_history,
            )
            .await
    }

    pub(crate) async fn apply_toolcall_host_commit<
        AttemptOnce,
        AttemptOnceFuture,
        YieldRetry,
        YieldRetryFuture,
        FailClosed,
        FailClosedFuture,
        AbortPending,
        AbortPendingFuture,
    >(
        self,
        call_id: &str,
        current_turn_provider_input_tokens: Option<i64>,
        mut attempt_once: AttemptOnce,
        yield_retry: YieldRetry,
        fail_closed: FailClosed,
        abort_pending: AbortPending,
    ) -> Result<Option<CompletedToolCallHostOutcome>, SpineError>
    where
        AttemptOnce: FnMut(ToolcallHostCommitAttempt) -> AttemptOnceFuture,
        AttemptOnceFuture: Future<Output = Result<ToolcallHostAttempt, SpineError>>,
        YieldRetry: FnMut() -> YieldRetryFuture,
        YieldRetryFuture: Future<Output = ()>,
        FailClosed: FnMut(&'static str) -> FailClosedFuture,
        FailClosedFuture: Future<Output = ()>,
        AbortPending: FnMut(&'static str) -> AbortPendingFuture,
        AbortPendingFuture: Future<Output = ()>,
    {
        self.inner
            .apply_toolcall_host_commit(
                call_id,
                current_turn_provider_input_tokens,
                |attempt| {
                    let future = attempt_once(ToolcallHostCommitAttempt { inner: attempt });
                    async move { future.await.map(|attempt| attempt.inner) }
                },
                yield_retry,
                fail_closed,
                abort_pending,
            )
            .await
            .map(|outcome| outcome.map(|inner| CompletedToolCallHostOutcome { inner }))
    }

    pub(crate) fn apply_history_updates_or_keep(
        self,
        mut apply_history_update: impl FnMut(
            HistoryHostEffect,
        ) -> Result<Result<(), HistoryHostEffect>, String>,
    ) -> Result<Self, String> {
        self.inner
            .apply_history_updates_or_keep(|effect| {
                apply_history_update(HistoryHostEffect { inner: effect })
                    .map(|result| result.map_err(|effect| effect.inner))
            })
            .map(Self::from_runtime)
    }

    pub(crate) fn into_tree_host_updates(self) -> TreeHostUpdates {
        TreeHostUpdates {
            inner: self.inner.into_tree_host_updates(),
        }
    }

    pub(crate) async fn apply_root_compact_history_publication<
        E,
        PublishHistory,
        PublishHistoryFuture,
        FinalizeInstallFailure,
        FinalizeInstallFailureFuture,
        AfterInstalled,
        AfterInstalledFuture,
    >(
        self,
        state: Option<&tokio::sync::Mutex<SpineSessionState>>,
        native_items: Vec<ResponseItem>,
        is_fixed_prefix_item: impl Fn(&ResponseItem) -> bool,
        invariant_error: impl Fn(String) -> E,
        publish_history: PublishHistory,
        finalize_install_failure: FinalizeInstallFailure,
        after_installed: AfterInstalled,
    ) -> Result<Option<SpineTreeUpdateEvent>, E>
    where
        PublishHistory: FnOnce(Vec<ResponseItem>, bool) -> PublishHistoryFuture,
        PublishHistoryFuture: Future<Output = Result<(), E>>,
        FinalizeInstallFailure: FnOnce(String) -> FinalizeInstallFailureFuture,
        FinalizeInstallFailureFuture: Future<Output = E>,
        AfterInstalled: FnOnce() -> AfterInstalledFuture,
        AfterInstalledFuture: Future<Output = Result<(), E>>,
    {
        self.inner
            .apply_root_compact_history_publication(
                native_items,
                is_fixed_prefix_item,
                invariant_error,
                publish_history,
                |published_variable_history_len| async move {
                    let install_result = match state {
                        Some(state) => {
                            let mut guard = state.lock().await;
                            guard
                                .take_pending_root_compact_after_history_publish(
                                    published_variable_history_len,
                                )
                                .map(Some)
                                .map_err(|err| err.to_string())
                        }
                        None => {
                            Err("spine runtime missing before root compact PS install".to_string())
                        }
                    };
                    match install_result {
                        Ok(snapshot) => Ok(snapshot),
                        Err(reason) => Err(finalize_install_failure(reason).await),
                    }
                },
                after_installed,
            )
            .await
    }
}

impl TreeHostUpdates {
    pub(crate) fn into_parts(self) -> (Vec<SpineTreeUpdateEvent>, Vec<SpineTreeUpdateEvent>) {
        self.inner.into_parts()
    }
}

impl<'a> CompletedToolCallOutputEvidence<'a> {
    fn from_runtime(inner: super::runtime::SpineCompletedToolCallOutputEvidence<'a>) -> Self {
        Self { inner }
    }

    pub(crate) fn call_id(&self) -> &'a str {
        self.inner.call_id()
    }

    pub(crate) fn commit_output_item(&self) -> &'a ResponseItem {
        self.inner.commit_output_item()
    }

    pub(crate) fn single_output_requiring_optional_prerecord(
        &self,
    ) -> Option<(&'a str, &'a ResponseItem)> {
        self.inner.single_output_requiring_optional_prerecord()
    }

    pub(crate) fn output_group_to_record_before_commit(&self) -> Option<&'a [ResponseItem]> {
        self.inner.output_group_to_record_before_commit()
    }
}

impl CompletedToolCallHostOutcome {
    pub(crate) fn no_spine_commit() -> Self {
        Self {
            inner: super::runtime::SpineCompletedToolCallHostOutcome::no_spine_commit(),
        }
    }

    pub(crate) fn take_post_commit_effects(&mut self) -> HostEffects {
        HostEffects::from_runtime(self.inner.take_post_commit_effects())
    }

    pub(crate) fn set_deferred_tree_update(
        &mut self,
        deferred_tree_update: Option<SpineTreeUpdateEvent>,
    ) {
        self.inner.set_deferred_tree_update(deferred_tree_update);
    }

    pub(crate) fn take_deferred_tree_update(&mut self) -> Option<SpineTreeUpdateEvent> {
        self.inner.take_deferred_tree_update()
    }

    #[cfg(test)]
    pub(crate) fn into_test_parts(
        self,
    ) -> (
        super::runtime::SpineToolOutputRecording,
        Option<SpineTreeUpdateEvent>,
    ) {
        self.inner.into_test_parts()
    }
}

impl ToolcallHostCommitAttempt {
    pub(crate) fn attempt_completed_toolcall_commit(
        self,
        state: &mut SpineSessionState,
        tool_resp_item: &ResponseItem,
        tool_resp_already_recorded: bool,
        raw_items: &[Option<ResponseItem>],
        history_items: &[ResponseItem],
        expected_history: Vec<ResponseItem>,
        reference_context_item: Option<TurnContextItem>,
        apply_host_effects: impl FnOnce(HostEffects) -> Result<(), String>,
        build_snapshot: impl FnOnce(
            Option<(
                SpineTreeUpdateEvent,
                Vec<super::runtime::SpineOpenNodeContextProjection>,
            )>,
        ) -> Result<Option<SpineTreeUpdateEvent>, SpineError>,
    ) -> Result<ToolcallHostAttempt, SpineError> {
        let pre_compact_provider_input_tokens = self.inner.pre_compact_provider_input_tokens();
        let current_turn_provider_input_tokens = self.inner.current_turn_provider_input_tokens();
        let attempt = state.attempt_completed_toolcall_commit_with_host_effects(
            self.inner.into_commit_evidence(),
            tool_resp_item,
            tool_resp_already_recorded,
            raw_items,
            history_items,
            expected_history,
            reference_context_item,
            pre_compact_provider_input_tokens,
            current_turn_provider_input_tokens,
            |host_effects| apply_host_effects(HostEffects::from_runtime(host_effects)),
            build_snapshot,
        )?;
        Ok(ToolcallHostAttempt { inner: attempt })
    }
}

impl ToolcallHostAttempt {
    pub(crate) fn host_lock_busy() -> Self {
        Self {
            inner: super::runtime::SpineToolcallHostAttempt::host_lock_busy(),
        }
    }
}

impl SingleToolcallOutputRecordingPlan {
    pub(crate) fn raw_len(&self) -> u64 {
        self.inner.raw_len()
    }

    pub(crate) fn prerecord_output_before_reduce(&self) -> bool {
        self.inner.prerecord_output_before_reduce()
    }
}

impl GroupedToolcallOutputRecordingPlan {
    pub(crate) fn into_raw_ordinals(self) -> Vec<Option<u64>> {
        self.inner.into_raw_ordinals()
    }
}

impl<'a> ToolcallOutputRecordingRequest<'a> {
    fn into_runtime(self) -> super::runtime::SpineToolcallOutputRecordingRequest<'a> {
        match self {
            Self::Single { call_id, raw_items } => {
                super::runtime::SpineToolcallOutputRecordingRequest::Single { call_id, raw_items }
            }
            Self::Grouped { output_items } => {
                super::runtime::SpineToolcallOutputRecordingRequest::Grouped { output_items }
            }
        }
    }
}

impl ToolcallOutputRecordingPlan {
    fn from_runtime(inner: super::runtime::SpineToolcallOutputRecordingPlan) -> Self {
        match inner {
            super::runtime::SpineToolcallOutputRecordingPlan::Single(plan) => {
                Self::Single(plan.map(|inner| SingleToolcallOutputRecordingPlan { inner }))
            }
            super::runtime::SpineToolcallOutputRecordingPlan::Grouped(inner) => {
                Self::Grouped(GroupedToolcallOutputRecordingPlan { inner })
            }
        }
    }
}

impl ReplayRuntime {
    pub(crate) fn has_runtime(&self) -> bool {
        self.inner.has_runtime()
    }

    pub(crate) fn live_root_compacts(&self) -> &[super::runtime::LiveRootCompact] {
        self.inner.live_root_compacts()
    }

    pub(crate) fn into_materialized(self) -> Option<Vec<ResponseItem>> {
        self.inner.into_materialized()
    }
}

impl HistoryHostEffect {
    pub(crate) fn apply_history_update_or_self(
        self,
        current_history: &[ResponseItem],
        replace_history_suffix: impl FnOnce(
            std::ops::Range<usize>,
            Vec<ResponseItem>,
            Option<TurnContextItem>,
        ) -> Result<(), String>,
    ) -> Result<Result<(), Self>, String> {
        self.inner
            .apply_history_update_or_self(current_history, replace_history_suffix)
            .map(|result| result.map_err(|inner| Self { inner }))
    }
}

#[cfg(test)]
impl<'a> From<super::runtime::SpineToolCallEvidence<'a>> for ToolCallEvidence<'a> {
    fn from(evidence: super::runtime::SpineToolCallEvidence<'a>) -> Self {
        Self { inner: evidence }
    }
}

pub(crate) fn on_init(
    state: &mut SpineSessionState,
    evidence: InitEvidence<'_>,
) -> Result<HostEffects, SpineError> {
    state
        .on_init(super::runtime::SpineInitEvidence {
            rollout_path: evidence.rollout_path,
        })
        .map(HostEffects::from_runtime)
}

pub(crate) fn on_non_toolcall_msg(
    state: &mut SpineSessionState,
    evidence: MessageEvidence<'_>,
) -> Result<HostEffects, SpineError> {
    state
        .observe_non_toolcall_msg_with_host_effects(super::runtime::SpineMessageEvidence {
            rollout_path: evidence.rollout_path,
            raw_ordinal: evidence.raw_ordinal,
            context_index: evidence.context_index,
            item: evidence.item,
            raw_items: evidence.raw_items,
        })
        .map(HostEffects::from_runtime)
}

pub(crate) fn on_compact(
    state: &mut SpineSessionState,
    evidence: CompactEvidence<'_>,
) -> Result<HostEffects, SpineError> {
    state
        .prepare_native_root_compact_from_history_with_checkpoint(
            super::runtime::SpineCompactEvidence {
                rollout_path: evidence.rollout_path,
                compacted_history: evidence.compacted_history,
                raw_items: evidence.raw_items,
                close_provider_input_tokens: evidence.close_provider_input_tokens,
            },
        )
        .map(HostEffects::from_runtime)
}

pub(crate) fn on_toolcall(
    state: &mut SpineSessionState,
    evidence: ToolcallHookEvidence<'_>,
) -> Result<HostEffects, SpineError> {
    state
        .prepare_completed_toolcall_for_commit(super::runtime::SpineToolcallHookEvidence {
            completed_output: &evidence.completed_output.inner,
            output_raw_ordinals: evidence.output_raw_ordinals,
            output_context_start: evidence.output_context_start,
            raw_items: evidence.raw_items,
            current_turn_provider_input_tokens: evidence.current_turn_provider_input_tokens,
            tool_resp_already_recorded: evidence.tool_resp_already_recorded,
            recorded_inside_reduce: evidence.recorded_inside_reduce,
        })
        .map(HostEffects::from_runtime)
}

pub(crate) fn observe_toolcall_context_items(
    state: &mut SpineSessionState,
    items: &[ObservedContextItem<'_>],
    raw_items: &[Option<ResponseItem>],
) -> Result<(), SpineError> {
    state.observe_toolcall_context_item_facts(
        items
            .iter()
            .map(|item| (item.raw_ordinal, item.context_index, item.item)),
        raw_items,
    )
}

pub(crate) fn prepare_toolcall_output_recording(
    state: &SpineSessionState,
    request: ToolcallOutputRecordingRequest<'_>,
) -> Result<ToolcallOutputRecordingPlan, SpineError> {
    state
        .prepare_toolcall_output_recording(request.into_runtime())
        .map(ToolcallOutputRecordingPlan::from_runtime)
}

pub(crate) fn prepare_jit_replay_from_rollout_items(
    state: &SpineSessionState,
    rollout_path: &Path,
    raw_len: u64,
    raw_items: &[Option<ResponseItem>],
    rollback_cuts: &[usize],
) -> Result<ReplayRuntime, SpineError> {
    state
        .prepare_jit_replay_from_rollout_items(rollout_path, raw_len, raw_items, rollback_cuts)
        .map(|inner| ReplayRuntime { inner })
}

pub(crate) fn prepare_trim_replay_from_history(
    rollout_path: &Path,
    raw_len: u64,
    history_items: &[ResponseItem],
) -> Result<Option<ReplayRuntime>, SpineError> {
    SpineSessionState::prepare_trim_replay_from_history(rollout_path, raw_len, history_items)
        .map(|replay| replay.map(|inner| ReplayRuntime { inner }))
}

pub(crate) fn install_replay(
    state: &mut SpineSessionState,
    replay: ReplayRuntime,
) -> Result<Option<Vec<ResponseItem>>, SpineError> {
    state.install_replay(replay.inner)
}

pub(crate) fn materialized_history_host_effects_if_no_pending_tool_request(
    state: &SpineSessionState,
    raw_items: &[Option<ResponseItem>],
    expected_history: Vec<ResponseItem>,
    reference_context_item: Option<TurnContextItem>,
) -> Result<HostEffects, SpineError> {
    state
        .materialized_history_host_effects_if_no_pending_tool_request(
            raw_items,
            expected_history,
            reference_context_item,
        )
        .map(HostEffects::from_runtime)
}
