use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::TurnContextItem;
use codex_protocol::spine_tree::SpineTreeUpdateEvent;
use std::future::Future;

use super::super::runtime;
use super::super::runtime::SpineError;
use super::super::runtime::SpineSessionState;
use super::HostEffects;
use super::TreeSnapshotProjection;

pub(crate) struct CompletedToolCallHostOutcome {
    inner: runtime::SpineCompletedToolCallHostOutcome,
}

pub(crate) struct ToolcallHostCommitAttempt {
    inner: runtime::SpineToolcallHostCommitAttempt,
}

pub(crate) struct ToolcallHostCommitInput<'a> {
    attempt: ToolcallHostCommitAttempt,
    tool_resp_item: &'a ResponseItem,
    tool_resp_already_recorded: bool,
    raw_items: &'a [Option<ResponseItem>],
    expected_history: Vec<ResponseItem>,
}

pub(crate) struct ToolcallHostAttempt {
    inner: runtime::SpineToolcallHostAttempt,
}

#[cfg(test)]
pub(crate) type TestToolOutputRecording = runtime::SpineToolOutputRecording;

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
    pub(crate) fn into_test_parts(self) -> (TestToolOutputRecording, Option<SpineTreeUpdateEvent>) {
        self.inner.into_test_parts()
    }
}

impl ToolcallHostCommitAttempt {
    pub(crate) fn into_commit_input<'a>(
        self,
        tool_resp_item: &'a ResponseItem,
        tool_resp_already_recorded: bool,
        raw_items: &'a [Option<ResponseItem>],
        expected_history: Vec<ResponseItem>,
    ) -> ToolcallHostCommitInput<'a> {
        ToolcallHostCommitInput {
            attempt: self,
            tool_resp_item,
            tool_resp_already_recorded,
            raw_items,
            expected_history,
        }
    }
}

impl ToolcallHostCommitInput<'_> {
    pub(crate) fn attempt_completed_toolcall_commit(
        self,
        state: &mut SpineSessionState,
        history_items: &[ResponseItem],
        reference_context_item: Option<TurnContextItem>,
        apply_host_effects: impl FnOnce(HostEffects) -> Result<(), String>,
        build_snapshot: impl FnOnce(
            Option<TreeSnapshotProjection>,
        ) -> Result<Option<SpineTreeUpdateEvent>, SpineError>,
    ) -> Result<ToolcallHostAttempt, SpineError> {
        let pre_compact_provider_input_tokens =
            self.attempt.inner.pre_compact_provider_input_tokens();
        let current_turn_provider_input_tokens =
            self.attempt.inner.current_turn_provider_input_tokens();
        let attempt = state.attempt_completed_toolcall_commit_with_host_effects(
            self.attempt.inner.into_commit_evidence(),
            self.tool_resp_item,
            self.tool_resp_already_recorded,
            self.raw_items,
            history_items,
            self.expected_history,
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
