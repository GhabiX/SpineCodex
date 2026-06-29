use super::*;

#[path = "response_toolcall_fixtures.rs"]
mod response_toolcall_fixtures;
pub(crate) use response_toolcall_fixtures::*;
#[path = "response_output_fixtures.rs"]
mod response_output_fixtures;
pub(crate) use response_output_fixtures::*;
#[path = "response_item_fixtures.rs"]
mod response_item_fixtures;
pub(crate) use response_item_fixtures::*;

pub(crate) fn response_item_trace_signature(item: &ResponseItem) -> String {
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

pub(crate) fn materialized_trace_signature(
    runtime: &SpineRuntime,
    raw: &[Option<ResponseItem>],
) -> Vec<String> {
    runtime
        .materialize_variable_context_for_test(raw)
        .expect("materialize h(PS)")
        .iter()
        .map(response_item_trace_signature)
        .collect()
}
