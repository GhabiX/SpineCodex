use super::host_bridge::SPINE_INITIAL_CONTEXT_CLOSE_TAG;
use super::host_bridge::SPINE_INITIAL_CONTEXT_OPEN_TAG;
use super::host_bridge::parse_spine_initial_context_item;
use super::host_bridge::spine_memory_text_marker;
use super::ids::NodeId;
use super::store::SpineOperation;
use super::view::op_label;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use std::path::Path;

pub(crate) fn render_spine_memory_item(
    node_id: &NodeId,
    op: SpineOperation,
    summary: &str,
    memory_body: &str,
) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: format!(
                "{}\n## Spine Memory\n\nNode: {}\nOperation: {}\nSummary: {}\n\n{}",
                spine_memory_text_marker(node_id, op),
                node_id,
                op_label(op),
                summary,
                memory_body.trim()
            ),
        }],
        phase: None,
    }
}

pub(crate) fn render_spine_handoff_item(from_node: &NodeId, to_node: &NodeId) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "developer".to_string(),
        content: vec![ContentItem::InputText {
            text: format!(
                "<spine_handoff>\nSpine transition completed: {} -> {}; use {}'s generated memory as the current scope handoff. Spine Memory is internal context; never expose or imitate it in user-visible messages. Continue following preserved system, developer, and project instructions.\n\nTreat raw folded conversation as historical evidence, but treat unresolved user-facing conclusions, decisions, blockers, and next actions captured in the generated memory as current obligations. If the latest user request or generated memory indicates unfinished work, reconstruct the current scope state from the generated memory, latest user intent, and current evidence before continuing. Before asking for new instructions, answer or continue any pending latest user request using that context.\n</spine_handoff>",
                from_node, to_node, from_node
            ),
        }],
        phase: None,
    }
}

pub(crate) fn render_spine_initial_context_item(
    initial_context: Vec<ResponseItem>,
) -> CodexResult<ResponseItem> {
    let encoded = serde_json::to_string(&initial_context).map_err(|err| {
        CodexErr::Fatal(format!(
            "failed to encode spine initial context wrapper: {err}"
        ))
    })?;
    Ok(ResponseItem::Message {
        id: None,
        role: "developer".to_string(),
        content: vec![ContentItem::InputText {
            text: format!(
                "{SPINE_INITIAL_CONTEXT_OPEN_TAG}\n{encoded}\n{SPINE_INITIAL_CONTEXT_CLOSE_TAG}"
            ),
        }],
        phase: None,
    })
}

pub(crate) fn expand_spine_initial_context_items(items: &mut Vec<ResponseItem>) {
    let mut expanded = Vec::with_capacity(items.len());
    for item in std::mem::take(items) {
        if let Some(mut initial_context) = parse_spine_initial_context_item(&item) {
            expanded.append(&mut initial_context);
        } else {
            expanded.push(item);
        }
    }
    *items = expanded;
}

pub(crate) fn render_context_compacted_outline(
    scope_node_id: &NodeId,
    scope_summary: &str,
    base_path: Option<&Path>,
    child_rows: &[String],
) -> String {
    let mut rendered = String::new();
    rendered.push_str("## Context Compacted\n\n");
    if let Some(base_path) = base_path {
        rendered.push_str(&format!("Base: {}\n", base_path.display()));
    }
    rendered.push_str(&format!("[{}] {}\n", scope_node_id, scope_summary));
    for row in child_rows {
        rendered.push_str(&format!("|-- {}\n", row));
    }
    rendered
}
