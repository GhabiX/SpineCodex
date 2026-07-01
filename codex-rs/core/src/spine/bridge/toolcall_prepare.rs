use crate::context_manager::ContextManager;
use codex_protocol::models::ResponseItem;
use std::future::Future;

use super::super::hooks::toolcall::CompletedToolCallOutputEvidence;
use super::super::hooks::toolcall::ToolCallEvidence;
use super::super::hooks::toolcall::ToolcallHookEvidence;
use super::super::runtime::SpineError;
use super::toolcall_recording::GroupedToolcallOutputRecordingPlan;
use super::toolcall_recording::SingleToolcallOutputRecordingPlan;

struct SpineCompletedToolCallOutputAnchor {
    raw_ordinals: Vec<Option<u64>>,
    context_start: usize,
    already_recorded: bool,
    recorded_inside_reduce: bool,
    history_before_recorded_output: Option<ContextManager>,
}

pub(super) struct CompletedSpineToolCall<'a> {
    completed_output: CompletedToolCallOutputEvidence<'a>,
    output_raw_ordinals: Vec<Option<u64>>,
    output_context_start: usize,
    response_already_recorded: bool,
    response_recorded_inside_reduce: bool,
    history_before_recorded_output: Option<ContextManager>,
}

impl<'a> CompletedSpineToolCall<'a> {
    pub(super) fn host_commit_inputs(
        &self,
    ) -> (&'a str, &'a ResponseItem, bool, Option<&ContextManager>) {
        (
            self.completed_output.call_id(),
            self.completed_output.commit_output_item(),
            self.response_already_recorded,
            if self.response_recorded_inside_reduce {
                self.history_before_recorded_output.as_ref()
            } else {
                None
            },
        )
    }

    pub(super) fn hook_evidence<'b>(
        &'b self,
        raw_items: &'b [Option<ResponseItem>],
        current_turn_provider_input_tokens: Option<i64>,
    ) -> ToolcallHookEvidence<'b>
    where
        'a: 'b,
    {
        ToolcallHookEvidence::new(
            &self.completed_output,
            self.output_raw_ordinals.as_slice(),
            self.output_context_start,
            raw_items,
            current_turn_provider_input_tokens,
            self.response_already_recorded,
            self.response_recorded_inside_reduce,
        )
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
    let output_anchor =
        if let Some((call_id, item)) = output.single_output_requiring_optional_prerecord() {
            let Some(output_anchor) = record_single_output_if_needed(
                call_id,
                item,
                clone_history,
                raw_items_for_commit,
                prepare_single_recording,
                mutable_context_index_for_full_history_boundary,
                record_items,
            )
            .await?
            else {
                return Ok(None);
            };
            output_anchor
        } else if let Some((raw_ordinals, context_start)) =
            output.source_evidence_already_recorded_anchor()
        {
            SpineCompletedToolCallOutputAnchor {
                raw_ordinals: raw_ordinals.to_vec(),
                context_start,
                already_recorded: true,
                recorded_inside_reduce: false,
                history_before_recorded_output: None,
            }
        } else if let Some(output_items) = output.output_group_to_record_before_commit() {
            record_grouped_output_before_commit(
                &output,
                output_items,
                clone_history,
                raw_items_for_commit,
                prepare_grouped_recording,
                mutable_context_index_for_full_history_boundary,
                prevalidate_commit,
                record_items,
            )
            .await?
        } else {
            return Ok(None);
        };
    Ok(Some(CompletedSpineToolCall {
        completed_output: output,
        output_raw_ordinals: output_anchor.raw_ordinals,
        output_context_start: output_anchor.context_start,
        response_already_recorded: output_anchor.already_recorded,
        response_recorded_inside_reduce: output_anchor.recorded_inside_reduce,
        history_before_recorded_output: output_anchor.history_before_recorded_output,
    }))
}

async fn record_grouped_output_before_commit<
    'a,
    CloneHistory,
    CloneHistoryFuture,
    RawItemsForCommit,
    RawItemsForCommitFuture,
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
    mut raw_items_for_commit: RawItemsForCommit,
    mut prepare_grouped_recording: PrepareGroupedRecording,
    mutable_context_index_for_full_history_boundary: MutableContextIndexForFullHistoryBoundary,
    mut prevalidate_commit: PrevalidateCommit,
    mut record_items: RecordItems,
) -> Result<SpineCompletedToolCallOutputAnchor, SpineError>
where
    CloneHistory: FnMut() -> CloneHistoryFuture,
    CloneHistoryFuture: Future<Output = ContextManager>,
    RawItemsForCommit: FnMut() -> RawItemsForCommitFuture,
    RawItemsForCommitFuture: Future<Output = Result<Vec<Option<ResponseItem>>, SpineError>>,
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
    let history_before_recorded_output = clone_history().await;
    let history_items_before_recorded_output = history_before_recorded_output.raw_items();
    let output_recording_plan = prepare_grouped_recording(output_items.clone()).await?;
    let output_raw_ordinals = output_recording_plan.raw_ordinals;
    let raw_items = raw_items_for_commit().await?;
    validate_grouped_output_raw_ordinals(&output_raw_ordinals, raw_items.len())?;
    let live_mutable_len_before_output = mutable_context_index_for_full_history_boundary(
        history_items_before_recorded_output,
        history_items_before_recorded_output.len(),
    )?;
    let output_context_start = live_mutable_len_before_output;
    prevalidate_commit(*output, output_raw_ordinals.clone(), output_context_start).await?;
    record_items(output_items).await.map_err(|err| {
        SpineError::Operation(format!(
            "failed to record grouped Spine tool outputs before commit: {err}"
        ))
    })?;
    Ok(SpineCompletedToolCallOutputAnchor {
        raw_ordinals: output_raw_ordinals,
        context_start: output_context_start,
        already_recorded: true,
        recorded_inside_reduce: true,
        history_before_recorded_output: None,
    })
}

fn validate_grouped_output_raw_ordinals(
    output_raw_ordinals: &[Option<u64>],
    raw_items_len: usize,
) -> Result<(), SpineError> {
    let Some(first_raw_ordinal) = output_raw_ordinals.iter().copied().flatten().next() else {
        return Err(SpineError::InvalidEvent(
            "grouped Spine toolcall output missing raw ordinal".to_string(),
        ));
    };
    let first_raw_index = usize::try_from(first_raw_ordinal)
        .map_err(|_| SpineError::InvalidEvent("raw ordinal overflow".to_string()))?;
    if first_raw_index > raw_items_len {
        return Err(SpineError::InvalidEvent(format!(
            "grouped Spine toolcall output raw ordinal {first_raw_ordinal} exceeds raw trace length {raw_items_len}",
        )));
    }
    Ok(())
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
    let tool_resp_already_recorded = loop {
        history_for_output_anchor = clone_history().await;
        let history_items_for_output_anchor = history_for_output_anchor.raw_items();
        let raw_items = raw_items_for_commit().await?;
        let Some(recording_plan) = prepare_single_recording(call_id.to_string(), raw_items).await?
        else {
            return Ok(None);
        };
        raw_len = recording_plan.raw_len;
        let tool_resp_already_recorded =
            history_items_for_output_anchor.last() == Some(item) && raw_len > 0;
        if tool_resp_already_recorded {
            break true;
        }
        if recorded_output_inside_reduce {
            break false;
        }
        history_before_recorded_output = Some(history_for_output_anchor.clone());
        record_items(vec![item.clone()]).await.map_err(|err| {
            let kind = if recording_plan.prerecord_output_before_reduce {
                "close-like raw output"
            } else {
                "tool output"
            };
            SpineError::Operation(format!(
                "failed to record Spine {kind} before commit for call_id={call_id}: {err}"
            ))
        })?;
        recorded_output_inside_reduce = true;
    };
    let history_items_for_output_anchor = history_for_output_anchor.raw_items();
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
