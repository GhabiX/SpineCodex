use super::*;

#[path = "response_toolcall_fixtures.rs"]
mod response_toolcall_fixtures;
pub(super) use response_toolcall_fixtures::*;
#[path = "response_output_fixtures.rs"]
mod response_output_fixtures;
pub(super) use response_output_fixtures::*;

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
