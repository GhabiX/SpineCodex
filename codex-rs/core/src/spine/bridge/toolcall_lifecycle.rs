use crate::context_manager::ContextManager;
use codex_protocol::models::ResponseItem;
use std::future::Future;

use super::super::hooks;
use super::super::hooks::HostEffects;
use super::super::hooks::toolcall::CompletedToolCallOutputEvidence;
use super::super::hooks::toolcall::ToolCallEvidence;
use super::super::runtime::SpineError;
use super::super::runtime::SpineSessionState;
use super::toolcall_host_commit::CompletedToolCallHostOutcome;
use super::toolcall_host_commit::ToolcallHostAttempt;
use super::toolcall_host_commit::ToolcallHostCommitAttempt;
use super::toolcall_prepare;
use super::toolcall_prepare::CompletedSpineToolCall;
use super::toolcall_recording;
use super::toolcall_recording::GroupedToolcallOutputRecordingPlan;
use super::toolcall_recording::SingleToolcallOutputRecordingPlan;

pub(crate) struct ToolcallRuntime;

pub(crate) struct ToolcallPreparedHostCommit<'a> {
    inner: CompletedSpineToolCall<'a>,
}

impl<'a> ToolcallPreparedHostCommit<'a> {
    pub(crate) fn call_id(&self) -> String {
        self.inner.call_id().to_string()
    }

    pub(crate) fn response_item(&self) -> &'a ResponseItem {
        self.inner.response_item()
    }

    pub(crate) fn response_already_recorded(&self) -> bool {
        self.inner.response_already_recorded()
    }

    pub(crate) fn history_to_restore_on_commit_error(&self) -> Option<&ContextManager> {
        self.inner.history_to_restore_on_commit_error()
    }
}

pub(crate) struct ToolcallCommitPrevalidation<'a> {
    output: CompletedToolCallOutputEvidence<'a>,
    output_raw_ordinals: Vec<Option<u64>>,
    output_context_start: usize,
}

impl<'a> ToolcallCommitPrevalidation<'a> {
    fn new(
        output: CompletedToolCallOutputEvidence<'a>,
        output_raw_ordinals: Vec<Option<u64>>,
        output_context_start: usize,
    ) -> Self {
        Self {
            output,
            output_raw_ordinals,
            output_context_start,
        }
    }

    pub(crate) fn validate(
        self,
        state: &SpineSessionState,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<(), SpineError> {
        toolcall_prepare::prevalidate_output_for_commit(
            self.output,
            state,
            self.output_raw_ordinals.as_slice(),
            self.output_context_start,
            raw_items,
        )
    }
}

impl ToolcallRuntime {
    pub(crate) async fn prepare_completed_toolcall_for_commit<
        'a,
        CloneHistory,
        CloneHistoryFuture,
        RawItemsForCommit,
        RawItemsForCommitFuture,
        PrepareSingleRecording,
        PrepareSingleRecordingFuture,
        PrepareGroupedRecording,
        PrepareGroupedRecordingFuture,
        MutableContextIndexForFullHistoryBoundary,
        PrevalidateCommit,
        PrevalidateCommitFuture,
        RecordItems,
        RecordItemsFuture,
    >(
        evidence: &ToolCallEvidence<'a>,
        clone_history: CloneHistory,
        raw_items_for_commit: RawItemsForCommit,
        prepare_single_recording: PrepareSingleRecording,
        prepare_grouped_recording: PrepareGroupedRecording,
        mutable_context_index_for_full_history_boundary: MutableContextIndexForFullHistoryBoundary,
        prevalidate_commit: PrevalidateCommit,
        record_items: RecordItems,
    ) -> Result<Option<ToolcallPreparedHostCommit<'a>>, SpineError>
    where
        CloneHistory: FnMut() -> CloneHistoryFuture,
        CloneHistoryFuture: Future<Output = ContextManager>,
        RawItemsForCommit: FnMut() -> RawItemsForCommitFuture,
        RawItemsForCommitFuture: Future<Output = Result<Vec<Option<ResponseItem>>, SpineError>>,
        PrepareSingleRecording:
            FnMut(String, Vec<Option<ResponseItem>>) -> PrepareSingleRecordingFuture,
        PrepareSingleRecordingFuture:
            Future<Output = Result<Option<SingleToolcallOutputRecordingPlan>, SpineError>>,
        PrepareGroupedRecording: FnMut(Vec<ResponseItem>) -> PrepareGroupedRecordingFuture,
        PrepareGroupedRecordingFuture:
            Future<Output = Result<GroupedToolcallOutputRecordingPlan, SpineError>>,
        MutableContextIndexForFullHistoryBoundary:
            Fn(&[ResponseItem], usize) -> Result<usize, SpineError>,
        PrevalidateCommit: FnMut(ToolcallCommitPrevalidation<'a>) -> PrevalidateCommitFuture,
        PrevalidateCommitFuture: Future<Output = Result<(), SpineError>>,
        RecordItems: FnMut(Vec<ResponseItem>) -> RecordItemsFuture,
        RecordItemsFuture: Future<Output = Result<(), String>>,
    {
        let mut prevalidate_commit = prevalidate_commit;
        let prepared = toolcall_prepare::prepare_completed_toolcall_for_commit(
            evidence,
            clone_history,
            raw_items_for_commit,
            prepare_single_recording,
            prepare_grouped_recording,
            mutable_context_index_for_full_history_boundary,
            |output, output_raw_ordinals, output_context_start| {
                prevalidate_commit(ToolcallCommitPrevalidation::new(
                    output,
                    output_raw_ordinals,
                    output_context_start,
                ))
            },
            record_items,
        )
        .await?;
        Ok(prepared.map(|inner| ToolcallPreparedHostCommit { inner }))
    }

    pub(crate) fn prepare_single_output_recording(
        state: &SpineSessionState,
        call_id: &str,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<Option<SingleToolcallOutputRecordingPlan>, SpineError> {
        toolcall_recording::prepare_single_output_recording(state, call_id, raw_items)
    }

    pub(crate) fn prepare_grouped_output_recording(
        state: &SpineSessionState,
        output_items: &[ResponseItem],
    ) -> Result<GroupedToolcallOutputRecordingPlan, SpineError> {
        toolcall_recording::prepare_grouped_output_recording(state, output_items)
    }

    pub(crate) fn prepare_host_effects_for_commit(
        state: &mut SpineSessionState,
        toolcall: &ToolcallPreparedHostCommit<'_>,
        raw_items: &[Option<ResponseItem>],
        current_turn_provider_input_tokens: Option<i64>,
    ) -> Result<HostEffects, SpineError> {
        hooks::on_toolcall(
            state,
            toolcall
                .inner
                .hook_evidence(raw_items, current_turn_provider_input_tokens),
        )
    }

    pub(crate) async fn apply_host_commit<
        AttemptOnce,
        AttemptOnceFuture,
        YieldRetry,
        YieldRetryFuture,
        FailClosed,
        FailClosedFuture,
        AbortPending,
        AbortPendingFuture,
    >(
        effects: HostEffects,
        call_id: &str,
        current_turn_provider_input_tokens: Option<i64>,
        attempt_once: AttemptOnce,
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
        effects
            .apply_toolcall_host_commit(
                call_id,
                current_turn_provider_input_tokens,
                attempt_once,
                yield_retry,
                fail_closed,
                abort_pending,
            )
            .await
    }
}
