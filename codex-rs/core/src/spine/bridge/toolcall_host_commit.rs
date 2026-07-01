use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::TurnContextItem;
use codex_protocol::spine_tree::SpineTreeUpdateEvent;
use std::future::Future;

use super::super::hooks::HostEffects;
use super::super::runtime;
use super::super::runtime::SpineError;
use super::super::runtime::SpineSessionState;
use super::tree_projection::TreeSnapshotProjection;

pub(crate) struct CompletedToolCallHostOutcome {
    inner: runtime::SpineCompletedToolCallHostOutcome,
}

pub(crate) struct ToolcallHostCommitAttempt {
    inner: runtime::SpineToolcallHostCommitAttempt,
}

pub(crate) struct ToolcallHostAttempt {
    inner: runtime::SpineToolcallHostAttempt,
}

impl HostEffects {
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
}

impl CompletedToolCallHostOutcome {
    pub(crate) fn no_spine_commit() -> Self {
        Self {
            inner: runtime::SpineCompletedToolCallHostOutcome::no_spine_commit(),
        }
    }

    pub(crate) async fn apply_post_commit_effects_deferred<ApplyEffects, ApplyEffectsFuture>(
        &mut self,
        apply_effects: ApplyEffects,
    ) where
        ApplyEffects: FnOnce(HostEffects) -> ApplyEffectsFuture,
        ApplyEffectsFuture: Future<Output = Option<SpineTreeUpdateEvent>>,
    {
        let post_commit_effects = HostEffects::from_runtime(self.inner.take_post_commit_effects());
        let deferred_tree_update = apply_effects(post_commit_effects).await;
        self.inner.set_deferred_tree_update(deferred_tree_update);
    }

    pub(crate) async fn apply_post_commit_effects_and_emit<
        ApplyEffects,
        ApplyEffectsFuture,
        EmitDeferred,
        EmitDeferredFuture,
    >(
        &mut self,
        apply_effects: ApplyEffects,
        emit_deferred: EmitDeferred,
    ) where
        ApplyEffects: FnOnce(HostEffects) -> ApplyEffectsFuture,
        ApplyEffectsFuture: Future<Output = Option<SpineTreeUpdateEvent>>,
        EmitDeferred: FnOnce(SpineTreeUpdateEvent) -> EmitDeferredFuture,
        EmitDeferredFuture: Future<Output = ()>,
    {
        self.apply_post_commit_effects_deferred(apply_effects).await;
        if let Some(snapshot) = self.inner.take_deferred_tree_update() {
            emit_deferred(snapshot).await;
        }
    }

    #[cfg(test)]
    pub(crate) fn into_test_parts(
        self,
    ) -> (
        runtime::SpineToolOutputRecording,
        Option<SpineTreeUpdateEvent>,
    ) {
        self.inner.into_test_parts()
    }
}

impl ToolcallHostCommitAttempt {
    pub(crate) fn host_lock_busy(&self) -> ToolcallHostAttempt {
        ToolcallHostAttempt {
            inner: runtime::SpineToolcallHostAttempt::host_lock_busy(),
        }
    }

    pub(crate) fn attempt_with_host_state<'a>(
        self,
        tool_resp_item: &'a ResponseItem,
        tool_resp_already_recorded: bool,
        raw_items: &'a [Option<ResponseItem>],
        state: &mut SpineSessionState,
        history_items: &[ResponseItem],
        reference_context_item: Option<TurnContextItem>,
        expected_history: Vec<ResponseItem>,
        apply_host_effects: impl FnOnce(HostEffects) -> Result<(), String>,
        build_snapshot: impl FnOnce(
            Option<TreeSnapshotProjection>,
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
            |projection| build_snapshot(projection.map(TreeSnapshotProjection::from_runtime)),
        )?;
        Ok(ToolcallHostAttempt { inner: attempt })
    }
}

impl ToolcallHostAttempt {
    pub(crate) fn host_lock_busy() -> Self {
        Self {
            inner: runtime::SpineToolcallHostAttempt::host_lock_busy(),
        }
    }
}
