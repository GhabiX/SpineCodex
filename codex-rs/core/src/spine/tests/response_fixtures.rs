use super::*;

pub(super) fn text_item(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
    }
}

pub(super) fn anchored_text_item(anchor: u64, text: &str) -> ResponseItem {
    text_item(&format!("[U{anchor}]\n{text}"))
}

pub(super) fn multimodal_user_item() -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![
            ContentItem::InputText {
                text: "first text".to_string(),
            },
            ContentItem::InputImage {
                image_url: "data:image/png;base64,RAW_IMAGE_SHOULD_NOT_APPEAR".to_string(),
                detail: Some(ImageDetail::High),
            },
            ContentItem::InputText {
                text: "second text".to_string(),
            },
        ],
        phase: None,
    }
}

pub(super) fn assistant_text_item(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: text.to_string(),
        }],
        phase: None,
    }
}

pub(super) fn tool_req(raw_ordinal: u64, context_index: usize) -> ToolCallSegment {
    tool_segment(ToolCallSegmentKind::Request, raw_ordinal, context_index)
}

pub(super) fn tool_resp(raw_ordinal: u64, context_index: usize) -> ToolCallSegment {
    tool_segment(ToolCallSegmentKind::Response, raw_ordinal, context_index)
}

pub(super) fn tool_segment(
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

pub(super) fn completed_toolcall(
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

pub(super) fn event_tool_req(raw_ordinal: u64, context_index: u64) -> ToolCallEventSegment {
    event_tool_segment(ToolCallSegmentKind::Request, raw_ordinal, context_index)
}

pub(super) fn event_tool_resp(raw_ordinal: u64, context_index: u64) -> ToolCallEventSegment {
    event_tool_segment(ToolCallSegmentKind::Response, raw_ordinal, context_index)
}

pub(super) fn event_tool_segment(
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

pub(super) fn spine_call(name: &str, call_id: &str) -> ResponseItem {
    ResponseItem::FunctionCall {
        id: None,
        name: name.to_string(),
        namespace: Some(SPINE_NAMESPACE.to_string()),
        arguments: "{}".to_string(),
        call_id: call_id.to_string(),
    }
}

pub(super) fn ordinary_call(name: &str, call_id: &str) -> ResponseItem {
    ResponseItem::FunctionCall {
        id: None,
        name: name.to_string(),
        namespace: None,
        arguments: "{}".to_string(),
        call_id: call_id.to_string(),
    }
}

pub(super) fn function_output(call_id: &str) -> ResponseItem {
    function_output_text(call_id, "ok")
}

pub(super) fn function_output_text(call_id: &str, text: &str) -> ResponseItem {
    ResponseItem::FunctionCallOutput {
        call_id: call_id.to_string(),
        output: codex_protocol::models::FunctionCallOutputPayload::from_text(text.to_string()),
    }
}

pub(super) fn function_output_content_items(call_id: &str, text: &str) -> ResponseItem {
    ResponseItem::FunctionCallOutput {
        call_id: call_id.to_string(),
        output: codex_protocol::models::FunctionCallOutputPayload::from_content_items(vec![
            codex_protocol::models::FunctionCallOutputContentItem::InputText {
                text: text.to_string(),
            },
        ]),
    }
}

pub(super) fn function_output_text_content(item: &ResponseItem) -> &str {
    let ResponseItem::FunctionCallOutput { output, .. } = item else {
        panic!("expected FunctionCallOutput, got {item:?}");
    };
    output.text_content().expect("text output")
}

pub(super) fn response_item_trace_signature(item: &ResponseItem) -> String {
    match item {
        ResponseItem::Message { role, content, .. } => {
            let text = content
                .iter()
                .filter_map(|item| match item {
                    ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                        Some(text.as_str())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            if text.starts_with("<spine_memory>")
                && let Some(line) = text
                    .lines()
                    .find(|line| line.starts_with("# Spine Memory "))
            {
                return format!("memory:{line}");
            }
            let text = text
                .strip_prefix("[U")
                .and_then(|rest| rest.split_once("]\n").map(|(_, body)| body))
                .unwrap_or(&text);
            format!("{role}:{text}")
        }
        ResponseItem::FunctionCall {
            name,
            namespace,
            call_id,
            ..
        } => {
            if namespace.as_deref() == Some(SPINE_NAMESPACE) {
                format!("spine-call:{name}:{call_id}")
            } else {
                format!("tool-call:{name}:{call_id}")
            }
        }
        ResponseItem::FunctionCallOutput { call_id, output } => {
            let text = output.text_content().unwrap_or("<structured-output>");
            format!("tool-output:{call_id}:{text}")
        }
        other => format!("{other:?}"),
    }
}

pub(super) fn materialized_trace_signature(
    runtime: &SpineRuntime,
    raw: &[Option<ResponseItem>],
) -> Vec<String> {
    runtime
        .materialize_history(raw)
        .expect("materialize h(PS)")
        .iter()
        .map(response_item_trace_signature)
        .collect()
}

pub(super) fn custom_tool_output_text(call_id: &str, text: &str) -> ResponseItem {
    ResponseItem::CustomToolCallOutput {
        call_id: call_id.to_string(),
        name: Some("custom_tool".to_string()),
        output: codex_protocol::models::FunctionCallOutputPayload::from_text(text.to_string()),
    }
}

pub(super) fn custom_tool_output_text_content(item: &ResponseItem) -> &str {
    let ResponseItem::CustomToolCallOutput { output, .. } = item else {
        panic!("expected CustomToolCallOutput, got {item:?}");
    };
    output.text_content().expect("custom tool text output")
}

pub(super) fn manual_toolcall_event(
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
