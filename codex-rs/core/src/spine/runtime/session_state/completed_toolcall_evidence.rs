use codex_protocol::models::ResponseItem;
use codex_rollout::should_persist_response_item;
use std::collections::BTreeSet;

use super::super::CompletedToolCall;
use super::super::CompletedToolCallSegment;
use super::super::SpineError;
use super::super::support::tool_response_call_id;
use crate::spine::model::ToolCallSegmentKind;

#[derive(Clone, Debug)]
pub(crate) struct SpineCompletedToolCallEvidence {
    completed_toolcall: CompletedToolCall,
}

#[derive(Clone, Debug)]
pub(crate) struct SpineToolcallCommitEvidence {
    pub(super) call_id: String,
    pub(super) completed_toolcall: SpineCompletedToolCallEvidence,
    control_policy: SpineToolCallControlPolicy,
}

pub(crate) struct SpineToolCallEvidence<'a> {
    kind: SpineToolCallEvidenceKind<'a>,
    control_policy: SpineToolCallControlPolicy,
}

pub(crate) struct SpineToolcallHookEvidence<'a> {
    pub(crate) commit_evidence: &'a SpineToolcallCommitEvidence,
    pub(crate) raw_items: &'a [Option<ResponseItem>],
    pub(crate) current_turn_provider_input_tokens: Option<i64>,
    pub(crate) tool_resp_already_recorded: bool,
    pub(crate) recorded_inside_reduce: bool,
}

enum SpineToolCallEvidenceKind<'a> {
    Single {
        item: &'a ResponseItem,
    },
    Grouped {
        commit_call_id: &'a str,
        tool_call_ids: &'a [String],
        output_items: &'a [ResponseItem],
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpineToolCallControlPolicy {
    Normal,
    ForceOrdinary,
}

pub(crate) struct SpineCompletedToolCallOutputEvidence<'a> {
    call_id: &'a str,
    output_items: &'a [ResponseItem],
    commit_output_item: &'a ResponseItem,
    pub(super) request_call_ids: SpineCompletedToolCallRequestIds<'a>,
    recording: SpineToolCallOutputHostRecording,
    pub(super) control_policy: SpineToolCallControlPolicy,
}

pub(super) enum SpineCompletedToolCallRequestIds<'a> {
    Single(&'a str),
    Grouped(&'a [String]),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SpineToolCallOutputHostRecording {
    MaybePreRecordSingle,
    RecordGroupBeforeCommit,
}

impl<'a> SpineToolCallEvidence<'a> {
    pub(crate) fn single(item: &'a ResponseItem) -> Self {
        Self {
            kind: SpineToolCallEvidenceKind::Single { item },
            control_policy: SpineToolCallControlPolicy::Normal,
        }
    }

    pub(crate) fn grouped(
        commit_call_id: &'a str,
        tool_call_ids: &'a [String],
        output_items: &'a [ResponseItem],
    ) -> Self {
        Self::grouped_with_policy(
            commit_call_id,
            tool_call_ids,
            output_items,
            SpineToolCallControlPolicy::Normal,
        )
    }

    pub(crate) fn grouped_as_ordinary(
        commit_call_id: &'a str,
        tool_call_ids: &'a [String],
        output_items: &'a [ResponseItem],
    ) -> Self {
        Self::grouped_with_policy(
            commit_call_id,
            tool_call_ids,
            output_items,
            SpineToolCallControlPolicy::ForceOrdinary,
        )
    }

    fn grouped_with_policy(
        commit_call_id: &'a str,
        tool_call_ids: &'a [String],
        output_items: &'a [ResponseItem],
        control_policy: SpineToolCallControlPolicy,
    ) -> Self {
        Self {
            kind: SpineToolCallEvidenceKind::Grouped {
                commit_call_id,
                tool_call_ids,
                output_items,
            },
            control_policy,
        }
    }

    pub(crate) fn completed_output(
        &self,
    ) -> Result<Option<SpineCompletedToolCallOutputEvidence<'a>>, SpineError> {
        match &self.kind {
            SpineToolCallEvidenceKind::Single { item } => {
                let Some(call_id) = tool_response_call_id(item) else {
                    return Ok(None);
                };
                Ok(Some(SpineCompletedToolCallOutputEvidence {
                    call_id,
                    output_items: std::slice::from_ref(*item),
                    commit_output_item: *item,
                    request_call_ids: SpineCompletedToolCallRequestIds::Single(call_id),
                    recording: SpineToolCallOutputHostRecording::MaybePreRecordSingle,
                    control_policy: self.control_policy,
                }))
            }
            SpineToolCallEvidenceKind::Grouped {
                commit_call_id,
                tool_call_ids,
                output_items,
            } => {
                let commit_output_item =
                    validate_grouped_toolcall_outputs(commit_call_id, tool_call_ids, output_items)?;
                Ok(Some(SpineCompletedToolCallOutputEvidence {
                    call_id: *commit_call_id,
                    output_items: *output_items,
                    commit_output_item,
                    request_call_ids: SpineCompletedToolCallRequestIds::Grouped(*tool_call_ids),
                    recording: SpineToolCallOutputHostRecording::RecordGroupBeforeCommit,
                    control_policy: self.control_policy,
                }))
            }
        }
    }
}

impl<'a> SpineCompletedToolCallOutputEvidence<'a> {
    pub(crate) fn call_id(&self) -> &'a str {
        self.call_id
    }

    pub(crate) fn single_output_item(&self) -> Option<&'a ResponseItem> {
        matches!(
            self.recording,
            SpineToolCallOutputHostRecording::MaybePreRecordSingle
        )
        .then_some(self.commit_output_item)
    }

    pub(crate) fn commit_output_item(&self) -> &'a ResponseItem {
        self.commit_output_item
    }

    pub(crate) fn single_output_requiring_optional_prerecord(
        &self,
    ) -> Option<(&'a str, &'a ResponseItem)> {
        self.single_output_item().map(|item| (self.call_id, item))
    }

    pub(crate) fn output_group_to_record_before_commit(&self) -> Option<&'a [ResponseItem]> {
        match self.recording {
            SpineToolCallOutputHostRecording::MaybePreRecordSingle => None,
            SpineToolCallOutputHostRecording::RecordGroupBeforeCommit => Some(self.output_items),
        }
    }
}

impl SpineCompletedToolCallEvidence {
    fn new(completed_toolcall: CompletedToolCall) -> Self {
        Self { completed_toolcall }
    }

    pub(super) fn first_segment_context_index(&self) -> Result<usize, SpineError> {
        self.completed_toolcall
            .segments
            .first()
            .map(|segment| segment.context_index)
            .ok_or_else(|| {
                SpineError::InvalidEvent("completed toolcall missing first segment".to_string())
            })
    }

    pub(super) fn into_completed_toolcall(self) -> CompletedToolCall {
        self.completed_toolcall
    }
}

impl SpineToolcallCommitEvidence {
    pub(super) fn new(
        call_id: impl Into<String>,
        completed_toolcall: SpineCompletedToolCallEvidence,
    ) -> Self {
        Self {
            call_id: call_id.into(),
            completed_toolcall,
            control_policy: SpineToolCallControlPolicy::Normal,
        }
    }

    pub(super) fn with_control_policy(
        mut self,
        control_policy: SpineToolCallControlPolicy,
    ) -> Self {
        self.control_policy = control_policy;
        self
    }

    pub(super) fn force_ordinary(&self) -> bool {
        self.control_policy == SpineToolCallControlPolicy::ForceOrdinary
    }

    pub(crate) fn call_id(&self) -> &str {
        &self.call_id
    }
}

fn validate_grouped_toolcall_outputs<'a>(
    commit_call_id: &str,
    tool_call_ids: &[String],
    output_items: &'a [ResponseItem],
) -> Result<&'a ResponseItem, SpineError> {
    let expected_call_ids = tool_call_ids.iter().cloned().collect::<BTreeSet<_>>();
    let output_call_ids = collect_grouped_output_call_ids(output_items, &expected_call_ids)?;
    for call_id in tool_call_ids {
        if !output_call_ids.contains(call_id) {
            return Err(SpineError::InvalidEvent(format!(
                "grouped Spine toolcall missing output for call_id={call_id}"
            )));
        }
    }
    commit_output_item_for_group(commit_call_id, output_items)
}

fn collect_grouped_output_call_ids(
    output_items: &[ResponseItem],
    expected_call_ids: &BTreeSet<String>,
) -> Result<BTreeSet<String>, SpineError> {
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
    Ok(output_call_ids)
}

fn commit_output_item_for_group<'a>(
    commit_call_id: &str,
    output_items: &'a [ResponseItem],
) -> Result<&'a ResponseItem, SpineError> {
    output_items
        .iter()
        .find(|item| tool_response_call_id(item) == Some(commit_call_id))
        .ok_or_else(|| {
            SpineError::InvalidEvent(format!(
                "grouped Spine toolcall missing output for commit call_id={commit_call_id}"
            ))
        })
}

pub(super) fn completed_toolcall_request_segment(
    raw_ordinal: u64,
    context_index: usize,
) -> CompletedToolCallSegment {
    CompletedToolCallSegment {
        kind: ToolCallSegmentKind::Request,
        raw_ordinal,
        context_index,
    }
}

pub(super) fn completed_toolcall_response_segment(
    raw_ordinal: u64,
    context_index: usize,
) -> CompletedToolCallSegment {
    CompletedToolCallSegment {
        kind: ToolCallSegmentKind::Response,
        raw_ordinal,
        context_index,
    }
}

pub(super) fn completed_toolcall_request_segments(
    request_anchors: impl IntoIterator<Item = (u64, usize)>,
) -> Vec<CompletedToolCallSegment> {
    request_anchors
        .into_iter()
        .map(|(raw_ordinal, context_index)| {
            completed_toolcall_request_segment(raw_ordinal, context_index)
        })
        .collect()
}

pub(super) fn completed_toolcall_response_segments(
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

pub(super) fn assign_response_item_raw_ordinals(
    raw_start: u64,
    items: &[ResponseItem],
) -> Result<Vec<Option<u64>>, SpineError> {
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
    Ok(ordinals)
}

pub(super) fn completed_toolcall_evidence_from_segments(
    call_id: &str,
    request_call_ids: &[String],
    mut request_segments: Vec<CompletedToolCallSegment>,
    mut response_segments: Vec<CompletedToolCallSegment>,
    missing_request_error: &'static str,
    missing_response_error: &'static str,
) -> Result<SpineCompletedToolCallEvidence, SpineError> {
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
    Ok(SpineCompletedToolCallEvidence::new(CompletedToolCall {
        call_id: call_id.to_string(),
        request_call_ids: request_call_ids.to_vec(),
        segments,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment_tuple(segment: &CompletedToolCallSegment) -> (ToolCallSegmentKind, u64, usize) {
        (segment.kind, segment.raw_ordinal, segment.context_index)
    }

    #[test]
    fn single_completed_toolcall_evidence_orders_request_before_response() {
        let toolcall = completed_toolcall_evidence_from_segments(
            "call-a",
            &["call-a".to_string()],
            vec![completed_toolcall_request_segment(10, 5)],
            vec![completed_toolcall_response_segment(11, 6)],
            "completed toolcall must contain a request",
            "completed toolcall must contain a response",
        )
        .expect("single evidence");

        assert_eq!(toolcall.completed_toolcall.call_id, "call-a");
        assert_eq!(
            toolcall.completed_toolcall.request_call_ids,
            vec!["call-a".to_string()]
        );
        assert_eq!(
            toolcall
                .completed_toolcall
                .segments
                .iter()
                .map(segment_tuple)
                .collect::<Vec<_>>(),
            vec![
                (ToolCallSegmentKind::Request, 10, 5),
                (ToolCallSegmentKind::Response, 11, 6),
            ]
        );
    }

    #[test]
    fn grouped_completed_toolcall_evidence_sorts_requests_and_responses_separately() {
        let tool_call_ids = vec!["call-b".to_string(), "call-a".to_string()];
        let toolcall = completed_toolcall_evidence_from_segments(
            "call-a",
            &tool_call_ids,
            completed_toolcall_request_segments([(20, 9), (10, 3)]),
            completed_toolcall_response_segments(&[Some(31), Some(30)], 7),
            "completed grouped toolcall must contain at least one request",
            "completed grouped toolcall must contain at least one response",
        )
        .expect("grouped evidence");

        assert_eq!(toolcall.completed_toolcall.call_id, "call-a");
        assert_eq!(toolcall.completed_toolcall.request_call_ids, tool_call_ids);
        assert_eq!(
            toolcall
                .completed_toolcall
                .segments
                .iter()
                .map(segment_tuple)
                .collect::<Vec<_>>(),
            vec![
                (ToolCallSegmentKind::Request, 10, 3),
                (ToolCallSegmentKind::Request, 20, 9),
                (ToolCallSegmentKind::Response, 31, 7),
                (ToolCallSegmentKind::Response, 30, 8),
            ]
        );
    }
}
