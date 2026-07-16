use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::num_format::format_si_suffix;
use codex_protocol::protocol::RolloutItem;
use codex_spine_core::SpineProjection;

use super::effective_rollout;
use super::pressure;

#[derive(Clone, Debug, PartialEq, Eq)]
struct SpineStatusPromptSignal {
    cursor: String,
    node_summary: Option<String>,
    parent: Option<String>,
    parent_summary: Option<String>,
    cursor_node_context_tokens: Option<i64>,
    context_left_tokens: Option<i64>,
}

pub(crate) fn prompt_overlay(
    rollout: &[RolloutItem],
    context_left_tokens: Option<i64>,
) -> ResponseItem {
    let effective = effective_rollout(rollout);
    let projection =
        super::projection_from_effective_rollout(&effective, rollout, true, false).spine;
    let signal = status_prompt_signal(&projection, &effective, context_left_tokens);
    developer_prompt_overlay_item(format_spine_status_prompt_overlay(&signal))
}

fn status_prompt_signal(
    projection: &SpineProjection,
    effective_rollout: &[(usize, &RolloutItem)],
    context_left_tokens: Option<i64>,
) -> SpineStatusPromptSignal {
    let active_node = projection
        .nodes
        .iter()
        .find(|node| node.id == projection.cursor);
    let parent = active_node.and_then(|node| node.parent.clone());
    let parent_summary = parent.as_ref().and_then(|parent_id| {
        projection
            .nodes
            .iter()
            .find(|node| &node.id == parent_id)
            .and_then(|node| node.summary.clone())
    });
    let node_summary = active_node.and_then(|node| node.summary.clone());
    let pressures = pressure::project_from_effective(effective_rollout, projection);
    let cursor_node_context_tokens = pressures
        .get(&projection.cursor)
        .and_then(|pressure| pressure.context_tokens);

    SpineStatusPromptSignal {
        cursor: projection.cursor.to_string(),
        node_summary,
        parent: parent.map(|node_id| node_id.to_string()),
        parent_summary,
        cursor_node_context_tokens,
        context_left_tokens,
    }
}

fn developer_prompt_overlay_item(text: String) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "developer".to_string(),
        content: vec![ContentItem::InputText { text }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

fn format_optional_summary_attribute(summary: Option<&str>) -> String {
    match summary.map(str::trim).filter(|summary| !summary.is_empty()) {
        Some(summary) => escape_xml_attribute(summary),
        None => "none".to_string(),
    }
}

fn format_spine_status_prompt_overlay(signal: &SpineStatusPromptSignal) -> String {
    let cursor_node_context = signal
        .cursor_node_context_tokens
        .map(format_si_suffix)
        .unwrap_or_else(|| "unavailable".to_string());
    let context_left = signal
        .context_left_tokens
        .map(format_si_suffix)
        .unwrap_or_else(|| "unavailable".to_string());
    let summary = format_optional_summary_attribute(signal.node_summary.as_deref());
    let parent_summary = format_optional_summary_attribute(signal.parent_summary.as_deref());
    format!(
        r#"<spine_status cursor="{}" summary="{}" parent="{}" parent_summary="{}" cursor_context="{}" context_left="{}""#,
        signal.cursor,
        summary,
        signal.parent.as_deref().unwrap_or("none"),
        parent_summary,
        cursor_node_context,
        context_left,
    ) + " />"
}

fn escape_xml_attribute(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
