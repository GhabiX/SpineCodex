use crate::context_manager::ContextManager;
use codex_protocol::models::ResponseItem;
use std::future::Future;

use super::super::hooks;
use super::super::hooks::HostEffects;
use super::super::hooks::toolcall::CompletedToolCallOutputEvidence;
use super::super::hooks::toolcall::ToolCallEvidence;
use super::super::runtime::SpineError;
use super::super::runtime::SpineSessionState;
use super::toolcall_prepare;
use super::toolcall_prepare::CompletedSpineToolCall;
use super::toolcall_recording::GroupedToolcallOutputRecordingPlan;
use super::toolcall_recording::SingleToolcallOutputRecordingPlan;
use super::toolcall_recording::ToolcallOutputRecordingPlan;
use super::toolcall_recording::ToolcallOutputRecordingRequest;

pub(crate) struct ToolcallRuntime;

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
    ) -> Result<Option<CompletedSpineToolCall<'a>>, SpineError>
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
        toolcall_prepare::prepare_completed_toolcall_for_commit(
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
        .await
    }

    pub(crate) fn prepare_single_output_recording(
        state: &SpineSessionState,
        call_id: &str,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<Option<SingleToolcallOutputRecordingPlan>, SpineError> {
        match ToolcallOutputRecordingRequest::single(call_id, raw_items).prepare(state)? {
            ToolcallOutputRecordingPlan::Single(plan) => Ok(plan),
            ToolcallOutputRecordingPlan::Grouped(_) => Err(SpineError::Invariant(
                "single toolcall output recording requested grouped plan".to_string(),
            )),
        }
    }

    pub(crate) fn prepare_grouped_output_recording(
        state: &SpineSessionState,
        output_items: &[ResponseItem],
    ) -> Result<GroupedToolcallOutputRecordingPlan, SpineError> {
        match ToolcallOutputRecordingRequest::grouped(output_items).prepare(state)? {
            ToolcallOutputRecordingPlan::Grouped(plan) => Ok(plan),
            ToolcallOutputRecordingPlan::Single(_) => Err(SpineError::Invariant(
                "grouped toolcall output recording requested single plan".to_string(),
            )),
        }
    }

    pub(crate) fn prepare_host_effects_for_commit(
        state: &mut SpineSessionState,
        toolcall: &CompletedSpineToolCall<'_>,
        raw_items: &[Option<ResponseItem>],
        current_turn_provider_input_tokens: Option<i64>,
    ) -> Result<HostEffects, SpineError> {
        hooks::on_toolcall(
            state,
            toolcall.hook_evidence(raw_items, current_turn_provider_input_tokens),
        )
    }
}
