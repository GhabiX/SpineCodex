use super::*;

pub(crate) fn text_item(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
    }
}

pub(crate) fn anchored_text_item(anchor: u64, text: &str) -> ResponseItem {
    text_item(&format!("[U{anchor}]\n{text}"))
}

pub(crate) fn multimodal_user_item() -> ResponseItem {
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

pub(crate) fn assistant_text_item(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: text.to_string(),
        }],
        phase: None,
    }
}

pub(crate) fn spine_call(name: &str, call_id: &str) -> ResponseItem {
    ResponseItem::FunctionCall {
        id: None,
        name: name.to_string(),
        namespace: Some(SPINE_NAMESPACE.to_string()),
        arguments: default_spine_call_arguments(name),
        call_id: call_id.to_string(),
    }
}

fn default_spine_call_arguments(name: &str) -> String {
    match name {
        SPINE_TOOL_OPEN => r#"{"summary":"test spine open"}"#.to_string(),
        _ => "{}".to_string(),
    }
}

pub(crate) fn ordinary_call(name: &str, call_id: &str) -> ResponseItem {
    ResponseItem::FunctionCall {
        id: None,
        name: name.to_string(),
        namespace: None,
        arguments: "{}".to_string(),
        call_id: call_id.to_string(),
    }
}
