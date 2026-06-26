use super::*;

pub(crate) fn tool_req(raw_ordinal: u64, context_index: usize) -> ToolCallSegment {
    tool_segment(ToolCallSegmentKind::Request, raw_ordinal, context_index)
}

pub(crate) fn tool_resp(raw_ordinal: u64, context_index: usize) -> ToolCallSegment {
    tool_segment(ToolCallSegmentKind::Response, raw_ordinal, context_index)
}

pub(crate) fn tool_segment(
    kind: ToolCallSegmentKind,
    raw_ordinal: u64,
    context_index: usize,
) -> ToolCallSegment {
    ToolCallSegment {
        kind,
        seg: SegRef::ResponseItem {
            raw_ordinal,
            context_index,
        },
    }
}

pub(crate) fn completed_toolcall(
    call_id: &str,
    segments: Vec<ToolCallSegment>,
) -> CompletedToolCall {
    let request_count = segments
        .iter()
        .filter(|segment| segment.kind == ToolCallSegmentKind::Request)
        .count();
    CompletedToolCall {
        call_id: call_id.to_string(),
        request_call_ids: vec![call_id.to_string(); request_count],
        segments: segments
            .into_iter()
            .map(|segment| {
                let SegRef::ResponseItem {
                    raw_ordinal,
                    context_index,
                } = segment.seg
                else {
                    panic!("test helper only accepts raw response-item toolcall segments");
                };
                CompletedToolCallSegment {
                    kind: segment.kind,
                    raw_ordinal,
                    context_index,
                }
            })
            .collect(),
    }
}

pub(crate) fn single_request_response_toolcall(
    call_id: &str,
    request_raw: u64,
    request_context: usize,
    response_raw: u64,
    response_context: usize,
) -> CompletedToolCall {
    completed_toolcall(
        call_id,
        vec![
            tool_req(request_raw, request_context),
            tool_resp(response_raw, response_context),
        ],
    )
}

pub(crate) fn event_tool_req(raw_ordinal: u64, context_index: u64) -> ToolCallEventSegment {
    event_tool_segment(ToolCallSegmentKind::Request, raw_ordinal, context_index)
}

pub(crate) fn event_tool_resp(raw_ordinal: u64, context_index: u64) -> ToolCallEventSegment {
    event_tool_segment(ToolCallSegmentKind::Response, raw_ordinal, context_index)
}

pub(crate) fn event_tool_segment(
    kind: ToolCallSegmentKind,
    raw_ordinal: u64,
    context_index: u64,
) -> ToolCallEventSegment {
    ToolCallEventSegment {
        kind,
        raw_ordinal,
        context_index,
    }
}

pub(crate) fn manual_toolcall_event(
    request_raw: u64,
    request_index: u64,
    response_raw: u64,
    response_index: u64,
) -> SpineLedgerEvent {
    SpineLedgerEvent::ToolCall {
        segments: vec![
            event_tool_req(request_raw, request_index),
            event_tool_resp(response_raw, response_index),
        ],
    }
}
