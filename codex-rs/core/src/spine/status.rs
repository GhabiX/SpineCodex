use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::num_format::format_si_suffix;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::TokenUsageInfo;
use codex_spine_core::RawBoundary;
use codex_spine_core::SpineProjection;

use super::effective_rollout;

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
    token_info: Option<&TokenUsageInfo>,
    context_left_tokens: Option<i64>,
) -> ResponseItem {
    let effective = effective_rollout(rollout);
    let projection =
        super::projection_from_effective_rollout(&effective, rollout, true, false).spine;
    let signal = status_prompt_signal(&projection, &effective, token_info, context_left_tokens);
    developer_prompt_overlay_item(format_spine_status_prompt_overlay(&signal))
}

fn status_prompt_signal(
    projection: &SpineProjection,
    effective_rollout: &[(usize, &RolloutItem)],
    token_info: Option<&TokenUsageInfo>,
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
    let current_provider_input_tokens = token_info.and_then(|current| {
        let input_tokens = current.last_token_usage.input_tokens;
        (input_tokens > 0).then_some(input_tokens)
    });
    let open_provider_input_tokens =
        active_node.and_then(|node| provider_input_baseline_after(effective_rollout, node.start));
    let cursor_node_context_tokens = current_provider_input_tokens
        .zip(open_provider_input_tokens)
        .and_then(|(current, baseline)| current.checked_sub(baseline))
        .filter(|tokens| *tokens >= 0);

    SpineStatusPromptSignal {
        cursor: projection.cursor.to_string(),
        node_summary,
        parent: parent.map(|node_id| node_id.to_string()),
        parent_summary,
        cursor_node_context_tokens,
        context_left_tokens,
    }
}

fn provider_input_baseline_after(
    effective_rollout: &[(usize, &RolloutItem)],
    open_boundary: RawBoundary,
) -> Option<i64> {
    effective_rollout.iter().find_map(|(boundary, item)| {
        if *boundary <= open_boundary.0 as usize {
            return None;
        }
        let RolloutItem::EventMsg(EventMsg::TokenCount(event)) = item else {
            return None;
        };
        let input_tokens = event.info.as_ref()?.last_token_usage.input_tokens;
        (input_tokens > 0).then_some(input_tokens)
    })
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
