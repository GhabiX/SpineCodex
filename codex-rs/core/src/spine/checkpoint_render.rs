use super::host_bridge::HostBridgeProjection;
use super::host_bridge::SPINE_INITIAL_CONTEXT_CLOSE_TAG;
use super::host_bridge::SPINE_INITIAL_CONTEXT_OPEN_TAG;
use super::host_bridge::parse_spine_initial_context_item;
use super::host_bridge::spine_memory_synthetic_id;
use super::host_bridge::spine_memory_text_marker;
use super::ids::NodeId;
use super::segment::RawSpan;
use super::segment::Segment;
use super::segment::SegmentArtifacts;
use super::segment::span;
use super::store::InstalledCompactSpan;
use super::store::SpineOperation;
use super::view::op_label;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RenderPiOrigin {
    Raw(RawSpan),
    Mem(String),
    Note(String),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RenderedPiItem {
    pub(crate) origin: RenderPiOrigin,
    pub(crate) item: ResponseItem,
}

pub(crate) fn render_pi_bridge_replacement_history(
    history: &[ResponseItem],
    runtime_spans: &[InstalledCompactSpan],
    pi: &[Segment],
    artifacts: &SegmentArtifacts,
    mem_items: &BTreeMap<String, ResponseItem>,
    note_items: &BTreeMap<String, Vec<ResponseItem>>,
) -> CodexResult<Vec<ResponseItem>> {
    Ok(
        render_pi_bridge_items(history, runtime_spans, pi, artifacts, mem_items, note_items)?
            .into_iter()
            .map(|rendered| rendered.item)
            .collect(),
    )
}

pub(crate) fn render_pi_bridge_items(
    history: &[ResponseItem],
    runtime_spans: &[InstalledCompactSpan],
    pi: &[Segment],
    artifacts: &SegmentArtifacts,
    mem_items: &BTreeMap<String, ResponseItem>,
    note_items: &BTreeMap<String, Vec<ResponseItem>>,
) -> CodexResult<Vec<RenderedPiItem>> {
    let projection = HostBridgeProjection::build(history, runtime_spans)?;
    let mut rendered = Vec::new();
    for segment in pi {
        match segment {
            Segment::Raw(raw_span) => {
                let start_index = projection
                    .effective_index_for_raw_boundary(raw_span.start)
                    .ok_or_else(|| {
                        CodexErr::Fatal(format!(
                            "render(Pi) Raw {} start does not map to an effective index",
                            raw_span
                        ))
                    })?;
                let end_index = projection
                    .effective_index_for_raw_boundary(raw_span.end)
                    .ok_or_else(|| {
                        CodexErr::Fatal(format!(
                            "render(Pi) Raw {} end does not map to an effective index",
                            raw_span
                        ))
                    })?;
                if start_index > end_index {
                    return Err(CodexErr::Fatal(format!(
                        "render(Pi) Raw {} maps to inverted effective range [{start_index}, {end_index})",
                        raw_span
                    )));
                }
                rendered.extend(history[start_index..end_index].iter().cloned().map(|item| {
                    RenderedPiItem {
                        origin: RenderPiOrigin::Raw(*raw_span),
                        item,
                    }
                }));
            }
            Segment::Mem { compact_id } => {
                span(segment, artifacts).map_err(pi_render_error)?;
                let item = mem_items.get(compact_id).ok_or_else(|| {
                    CodexErr::Fatal(format!("render(Pi) missing Mem item for {compact_id}"))
                })?;
                rendered.push(RenderedPiItem {
                    origin: RenderPiOrigin::Mem(compact_id.clone()),
                    item: item.clone(),
                });
            }
            Segment::Note { kind } => {
                let items = note_items.get(kind).ok_or_else(|| {
                    CodexErr::Fatal(format!("render(Pi) missing Note item for {kind}"))
                })?;
                rendered.extend(items.iter().cloned().map(|item| RenderedPiItem {
                    origin: RenderPiOrigin::Note(kind.clone()),
                    item,
                }));
            }
        }
    }
    Ok(rendered)
}

fn pi_render_error(error: impl std::fmt::Display) -> CodexErr {
    CodexErr::Fatal(format!(
        "render(Pi) failed to build canonical cover: {error}"
    ))
}

pub(crate) fn render_spine_memory_item(
    node_id: &NodeId,
    op: SpineOperation,
    summary: &str,
    memory_body: &str,
) -> ResponseItem {
    ResponseItem::Message {
        id: Some(spine_memory_synthetic_id(node_id, op)),
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
                "<spine_handoff>\nSpine transition completed: {} -> {}; use {}'s generated memory as the active-turn handoff. Spine Memory is internal context; never expose or imitate it in user-visible messages. Continue following preserved system, developer, and project instructions.\n\nTreat raw folded conversation as historical evidence, but treat unresolved user-facing conclusions, decisions, blockers, and next actions captured in the generated memory as current obligations. If the latest user request or generated memory indicates unfinished work, reconstruct the current node plan from the generated memory, latest user intent, and current evidence before continuing. Before asking for new instructions, answer or continue any pending latest user request using that context.\n</spine_handoff>",
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
    base_path: &Path,
    scope_memory_path: &Path,
    child_rows: &[(String, String)],
) -> String {
    let mut rendered = String::new();
    rendered.push_str("## Context Compacted\n\n");
    rendered.push_str(&format!("Base: {}\n", base_path.display()));
    rendered.push_str(&format!(
        "[{}] {} ({})\n",
        scope_node_id,
        scope_summary,
        scope_memory_path.display()
    ));
    for (summary, path) in child_rows {
        rendered.push_str(&format!("|-- {} ({})\n", summary, path));
    }
    rendered
}

pub(crate) fn render_slim_context_compacted_outline(
    scope_node_id: &NodeId,
    scope_summary: &str,
    child_rows: &[String],
) -> String {
    let mut rendered = String::new();
    rendered.push_str("## Context Compacted\n\n");
    rendered.push_str(&format!("[{}] {}\n", scope_node_id, scope_summary));
    for row in child_rows {
        rendered.push_str(&format!("|-- {}\n", row));
    }
    rendered
}
