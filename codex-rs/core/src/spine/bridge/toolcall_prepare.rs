use crate::context_manager::ContextManager;
use codex_protocol::models::ResponseItem;
use std::future::Future;

use super::super::hooks::toolcall::CompletedToolCallOutputEvidence;
use super::super::hooks::toolcall::ToolCallEvidence;
use super::super::runtime;
use super::super::runtime::SpineError;
use super::toolcall_recording::GroupedToolcallOutputRecordingPlan;
use super::toolcall_recording::SingleToolcallOutputRecordingPlan;

pub(crate) struct SpinePreparedToolCallEvidence<'a> {
    pub(crate) response_item: &'a ResponseItem,
    pub(crate) completed_output: CompletedToolCallOutputEvidence<'a>,
    pub(crate) output_raw_ordinals: Vec<Option<u64>>,
    pub(crate) output_context_start: usize,
}

pub(crate) struct SpineToolCallHostRecording {
    pub(crate) response_already_recorded: bool,
    pub(crate) response_recorded_inside_reduce: bool,
    pub(crate) history_before_recorded_output: Option<ContextManager>,
}

struct SpineCompletedToolCallOutputAnchor {
    raw_ordinals: Vec<Option<u64>>,
    context_start: usize,
    already_recorded: bool,
    recorded_inside_reduce: bool,
    history_before_recorded_output: Option<ContextManager>,
}

pub(crate) struct CompletedSpineToolCall<'a> {
    pub(crate) evidence: SpinePreparedToolCallEvidence<'a>,
    pub(crate) host_recording: SpineToolCallHostRecording,
}

impl SpineToolCallHostRecording {
    pub(crate) fn response_already_recorded(&self) -> bool {
        self.response_already_recorded
    }

    pub(crate) fn response_recorded_inside_reduce(&self) -> bool {
        self.response_recorded_inside_reduce
    }

    pub(crate) fn history_to_restore_on_commit_error(&self) -> Option<&ContextManager> {
        if self.response_recorded_inside_reduce {
            self.history_before_recorded_output.as_ref()
        } else {
            None
        }
    }
}

pub(super) async fn prepare_completed_toolcall_for_commit<
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
    PrevalidateCommit: FnMut(
        CompletedToolCallOutputEvidence<'a>,
        Vec<Option<u64>>,
        usize,
    ) -> PrevalidateCommitFuture,
    PrevalidateCommitFuture: Future<Output = Result<(), SpineError>>,
    RecordItems: FnMut(Vec<ResponseItem>) -> RecordItemsFuture,
    RecordItemsFuture: Future<Output = Result<(), String>>,
{
    let Some(output) = evidence.completed_output()? else {
        return Ok(None);
    };
    prepare_output_for_commit(
        output,
        clone_history,
        raw_items_for_commit,
        prepare_single_recording,
        prepare_grouped_recording,
        mutable_context_index_for_full_history_boundary,
        prevalidate_commit,
        record_items,
    )
    .await
}

pub(super) fn prevalidate_output_for_commit(
    output: CompletedToolCallOutputEvidence<'_>,
    state: &runtime::SpineSessionState,
    output_raw_ordinals: &[Option<u64>],
    output_context_start: usize,
    raw_items: &[Option<ResponseItem>],
) -> Result<(), SpineError> {
    let _ = state.completed_toolcall_commit_evidence_from_output(
        output.runtime_output(),
        output_raw_ordinals,
        output_context_start,
        raw_items,
    )?;
    Ok(())
}

async fn prepare_output_for_commit<
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
    output: CompletedToolCallOutputEvidence<'a>,
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
    PrevalidateCommit: FnMut(
        CompletedToolCallOutputEvidence<'a>,
        Vec<Option<u64>>,
        usize,
    ) -> PrevalidateCommitFuture,
    PrevalidateCommitFuture: Future<Output = Result<(), SpineError>>,
    RecordItems: FnMut(Vec<ResponseItem>) -> RecordItemsFuture,
    RecordItemsFuture: Future<Output = Result<(), String>>,
{
    let Some(output_anchor) = record_output_if_needed(
        &output,
        clone_history,
        raw_items_for_commit,
        prepare_single_recording,
        prepare_grouped_recording,
        mutable_context_index_for_full_history_boundary,
        prevalidate_commit,
        record_items,
    )
    .await?
    else {
        return Ok(None);
    };
    let response_item = output.commit_output_item();
    Ok(Some(CompletedSpineToolCall {
        evidence: SpinePreparedToolCallEvidence {
            response_item,
            completed_output: output,
            output_raw_ordinals: output_anchor.raw_ordinals,
            output_context_start: output_anchor.context_start,
        },
        host_recording: SpineToolCallHostRecording {
            response_already_recorded: output_anchor.already_recorded,
            response_recorded_inside_reduce: output_anchor.recorded_inside_reduce,
            history_before_recorded_output: output_anchor.history_before_recorded_output,
        },
    }))
}

async fn record_output_if_needed<
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
    output: &CompletedToolCallOutputEvidence<'a>,
    clone_history: CloneHistory,
    raw_items_for_commit: RawItemsForCommit,
    prepare_single_recording: PrepareSingleRecording,
    prepare_grouped_recording: PrepareGroupedRecording,
    mutable_context_index_for_full_history_boundary: MutableContextIndexForFullHistoryBoundary,
    prevalidate_commit: PrevalidateCommit,
    record_items: RecordItems,
) -> Result<Option<SpineCompletedToolCallOutputAnchor>, SpineError>
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
    PrevalidateCommit: FnMut(
        CompletedToolCallOutputEvidence<'a>,
        Vec<Option<u64>>,
        usize,
    ) -> PrevalidateCommitFuture,
    PrevalidateCommitFuture: Future<Output = Result<(), SpineError>>,
    RecordItems: FnMut(Vec<ResponseItem>) -> RecordItemsFuture,
    RecordItemsFuture: Future<Output = Result<(), String>>,
{
    if let Some((call_id, item)) = output.single_output_requiring_optional_prerecord() {
        return record_single_output_if_needed(
            call_id,
            item,
            clone_history,
            raw_items_for_commit,
            prepare_single_recording,
            mutable_context_index_for_full_history_boundary,
            record_items,
        )
        .await;
    }
    let Some(output_items) = output.output_group_to_record_before_commit() else {
        return Ok(None);
    };
    record_grouped_output_before_commit(
        output,
        output_items,
        clone_history,
        prepare_grouped_recording,
        mutable_context_index_for_full_history_boundary,
        prevalidate_commit,
        record_items,
    )
    .await
}

async fn record_grouped_output_before_commit<
    'a,
    CloneHistory,
    CloneHistoryFuture,
    PrepareGroupedRecording,
    PrepareGroupedRecordingFuture,
    MutableContextIndexForFullHistoryBoundary,
    PrevalidateCommit,
    PrevalidateCommitFuture,
    RecordItems,
    RecordItemsFuture,
>(
    output: &CompletedToolCallOutputEvidence<'a>,
    output_items: &[ResponseItem],
    mut clone_history: CloneHistory,
    mut prepare_grouped_recording: PrepareGroupedRecording,
    mutable_context_index_for_full_history_boundary: MutableContextIndexForFullHistoryBoundary,
    mut prevalidate_commit: PrevalidateCommit,
    mut record_items: RecordItems,
) -> Result<Option<SpineCompletedToolCallOutputAnchor>, SpineError>
where
    CloneHistory: FnMut() -> CloneHistoryFuture,
    CloneHistoryFuture: Future<Output = ContextManager>,
    PrepareGroupedRecording: FnMut(Vec<ResponseItem>) -> PrepareGroupedRecordingFuture,
    PrepareGroupedRecordingFuture:
        Future<Output = Result<GroupedToolcallOutputRecordingPlan, SpineError>>,
    MutableContextIndexForFullHistoryBoundary:
        Fn(&[ResponseItem], usize) -> Result<usize, SpineError>,
    PrevalidateCommit: FnMut(
        CompletedToolCallOutputEvidence<'a>,
        Vec<Option<u64>>,
        usize,
    ) -> PrevalidateCommitFuture,
    PrevalidateCommitFuture: Future<Output = Result<(), SpineError>>,
    RecordItems: FnMut(Vec<ResponseItem>) -> RecordItemsFuture,
    RecordItemsFuture: Future<Output = Result<(), String>>,
{
    let output_items = output_items.to_vec();
    let output_recording_plan = prepare_grouped_recording(output_items.clone()).await?;
    let history = clone_history().await;
    let output_context_start = mutable_context_index_for_full_history_boundary(
        history.raw_items(),
        history.raw_items().len(),
    )?;
    prevalidate_commit(
        *output,
        output_recording_plan.raw_ordinals().to_vec(),
        output_context_start,
    )
    .await?;
    record_items(output_items).await.map_err(|err| {
        SpineError::Operation(format!(
            "failed to record grouped Spine tool outputs before commit: {err}"
        ))
    })?;
    Ok(Some(SpineCompletedToolCallOutputAnchor {
        raw_ordinals: output_recording_plan.into_raw_ordinals(),
        context_start: output_context_start,
        already_recorded: true,
        recorded_inside_reduce: true,
        history_before_recorded_output: None,
    }))
}

async fn record_single_output_if_needed<
    CloneHistory,
    CloneHistoryFuture,
    RawItemsForCommit,
    RawItemsForCommitFuture,
    PrepareSingleRecording,
    PrepareSingleRecordingFuture,
    MutableContextIndexForFullHistoryBoundary,
    RecordItems,
    RecordItemsFuture,
>(
    call_id: &str,
    item: &ResponseItem,
    mut clone_history: CloneHistory,
    mut raw_items_for_commit: RawItemsForCommit,
    mut prepare_single_recording: PrepareSingleRecording,
    mutable_context_index_for_full_history_boundary: MutableContextIndexForFullHistoryBoundary,
    mut record_items: RecordItems,
) -> Result<Option<SpineCompletedToolCallOutputAnchor>, SpineError>
where
    CloneHistory: FnMut() -> CloneHistoryFuture,
    CloneHistoryFuture: Future<Output = ContextManager>,
    RawItemsForCommit: FnMut() -> RawItemsForCommitFuture,
    RawItemsForCommitFuture: Future<Output = Result<Vec<Option<ResponseItem>>, SpineError>>,
    PrepareSingleRecording:
        FnMut(String, Vec<Option<ResponseItem>>) -> PrepareSingleRecordingFuture,
    PrepareSingleRecordingFuture:
        Future<Output = Result<Option<SingleToolcallOutputRecordingPlan>, SpineError>>,
    MutableContextIndexForFullHistoryBoundary:
        Fn(&[ResponseItem], usize) -> Result<usize, SpineError>,
    RecordItems: FnMut(Vec<ResponseItem>) -> RecordItemsFuture,
    RecordItemsFuture: Future<Output = Result<(), String>>,
{
    let mut recorded_output_inside_reduce = false;
    let mut history_before_recorded_output = None;
    let mut raw_len;
    let mut history_for_output_anchor;
    loop {
        history_for_output_anchor = clone_history().await;
        let history_items_for_output_anchor = history_for_output_anchor.raw_items();
        let raw_items = raw_items_for_commit().await?;
        let Some(recording_plan) = prepare_single_recording(call_id.to_string(), raw_items).await?
        else {
            return Ok(None);
        };
        raw_len = recording_plan.raw_len();
        let tool_resp_already_recorded =
            history_items_for_output_anchor.last() == Some(item) && raw_len > 0;
        if tool_resp_already_recorded || recorded_output_inside_reduce {
            break;
        }
        history_before_recorded_output = Some(history_for_output_anchor.clone());
        record_items(vec![item.clone()]).await.map_err(|err| {
            let kind = if recording_plan.prerecord_output_before_reduce() {
                "close-like raw output"
            } else {
                "tool output"
            };
            SpineError::Operation(format!(
                "failed to record Spine {kind} before commit for call_id={call_id}: {err}"
            ))
        })?;
        recorded_output_inside_reduce = true;
    }
    let history_items_for_output_anchor = history_for_output_anchor.raw_items();
    let tool_resp_already_recorded =
        history_items_for_output_anchor.last() == Some(item) && raw_len > 0;
    let (tool_resp_raw_ordinal, tool_resp_full_history_index) = if tool_resp_already_recorded {
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
    let tool_resp_context_index = mutable_context_index_for_full_history_boundary(
        history_items_for_output_anchor,
        tool_resp_full_history_index,
    )?;
    Ok(Some(SpineCompletedToolCallOutputAnchor {
        raw_ordinals: vec![Some(tool_resp_raw_ordinal)],
        context_start: tool_resp_context_index,
        already_recorded: tool_resp_already_recorded,
        recorded_inside_reduce: recorded_output_inside_reduce,
        history_before_recorded_output,
    }))
}
