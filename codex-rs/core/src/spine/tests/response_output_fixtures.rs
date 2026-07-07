use super::*;

pub(crate) fn function_output(call_id: &str) -> ResponseItem {
    function_output_text(call_id, "ok")
}

pub(crate) fn function_output_text(call_id: &str, text: &str) -> ResponseItem {
    ResponseItem::FunctionCallOutput {
        call_id: call_id.to_string(),
        output: codex_protocol::models::FunctionCallOutputPayload::from_text(text.to_string()),
    }
}

pub(crate) fn trim_candidate_text(fragment: &str) -> String {
    assert!(!fragment.is_empty());
    let target_bytes = crate::spine::model::TOOL_RESPONSE_TRIM_THRESHOLD_BYTES as usize + 1_024;
    let repeat_count = (target_bytes / fragment.len()) + 1;
    fragment.repeat(repeat_count)
}

pub(crate) fn failed_function_output_text(call_id: &str, text: &str) -> ResponseItem {
    ResponseItem::FunctionCallOutput {
        call_id: call_id.to_string(),
        output: codex_protocol::models::FunctionCallOutputPayload {
            body: codex_protocol::models::FunctionCallOutputBody::Text(text.to_string()),
            success: Some(false),
        },
    }
}

pub(crate) fn function_output_content_items(call_id: &str, text: &str) -> ResponseItem {
    ResponseItem::FunctionCallOutput {
        call_id: call_id.to_string(),
        output: codex_protocol::models::FunctionCallOutputPayload::from_content_items(vec![
            codex_protocol::models::FunctionCallOutputContentItem::InputText {
                text: text.to_string(),
            },
        ]),
    }
}

pub(crate) fn function_output_text_content(item: &ResponseItem) -> &str {
    let ResponseItem::FunctionCallOutput { output, .. } = item else {
        panic!("expected FunctionCallOutput, got {item:?}");
    };
    output.text_content().expect("text output")
}

pub(crate) fn custom_tool_output_text(call_id: &str, text: &str) -> ResponseItem {
    ResponseItem::CustomToolCallOutput {
        call_id: call_id.to_string(),
        name: Some("custom_tool".to_string()),
        output: codex_protocol::models::FunctionCallOutputPayload::from_text(text.to_string()),
    }
}

pub(crate) fn custom_tool_output_text_content(item: &ResponseItem) -> &str {
    let ResponseItem::CustomToolCallOutput { output, .. } = item else {
        panic!("expected CustomToolCallOutput, got {item:?}");
    };
    output.text_content().expect("custom tool text output")
}
