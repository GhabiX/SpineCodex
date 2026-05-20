use super::candidate_mem_plan::CandidateMemPlan;
use super::host_bridge::HostBridgeProjection;
use super::host_bridge::SPINE_INITIAL_CONTEXT_CLOSE_TAG;
use super::host_bridge::SPINE_INITIAL_CONTEXT_OPEN_TAG;
use super::host_bridge::parse_spine_initial_context_item;
use super::host_bridge::spine_memory_text_marker;
use super::host_materialization::SpineHostMaterializationInput;
use super::host_materialization::SpineHostRawSource;
use super::host_materialization::insert_spine_host_note_segments;
use super::host_materialization::materialize_spine_host_history;
use super::ids::NodeId;
use super::store::InstalledCompactSpan;
use super::store::NotePlacement;
use super::store::SpineOperation;
use super::view::op_label;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use std::collections::BTreeMap;
use std::path::Path;

pub(crate) fn build_suffix_replacement_history_from_candidate_plan(
    old_history: &[ResponseItem],
    runtime_spans: &[InstalledCompactSpan],
    compact_id: &str,
    candidate_plan: &CandidateMemPlan,
    memory_item: ResponseItem,
    note_items: Vec<ResponseItem>,
) -> CodexResult<Vec<ResponseItem>> {
    let mut pi = candidate_plan.pi.clone();
    let artifacts = &candidate_plan.artifacts;
    let note_items = note_items
        .into_iter()
        .enumerate()
        .map(|(index, item)| (format!("suffix_note_{index}"), vec![item]))
        .collect::<BTreeMap<_, _>>();
    pi = insert_spine_host_note_segments(
        pi,
        compact_id,
        note_items.keys().cloned().collect(),
        NotePlacement::AfterMem,
    )?;
    let mut mem_items = memory_items_from_runtime_spans(old_history, runtime_spans)?;
    mem_items.insert(compact_id.to_string(), memory_item);
    materialize_spine_host_history(SpineHostMaterializationInput {
        pi: &pi,
        artifacts,
        raw_source: SpineHostRawSource::HostBridge {
            history: old_history,
            runtime_spans,
        },
        mem_items: &mem_items,
        note_items: &note_items,
    })
}

pub(crate) fn resolve_root_archive_cut(
    history: &[ResponseItem],
    planned_cut_index: usize,
    fold_end_ordinal: u64,
    runtime_spans: &[InstalledCompactSpan],
) -> CodexResult<(u64, usize)> {
    let projection = HostBridgeProjection::build(history, runtime_spans)?;
    let (archive_cut_ordinal, archive_cut_index) =
        projection
            .first_span_in_prefix(planned_cut_index)
            .map_or_else(
                || {
                    projection
                        .raw_for_effective_index(planned_cut_index)
                        .map(|raw| (raw, planned_cut_index))
                        .ok_or_else(|| {
                            CodexErr::Fatal(format!(
                                "spine root archive render(Pi) planned cut index {planned_cut_index} does not map to a raw ordinal"
                            ))
                        })
                },
                Ok,
            )?;
    if fold_end_ordinal < archive_cut_ordinal {
        return Err(CodexErr::Fatal(format!(
            "spine root archive render(Pi) fold_end ordinal {fold_end_ordinal} is before archive cut ordinal {archive_cut_ordinal}"
        )));
    }
    Ok((archive_cut_ordinal, archive_cut_index))
}

pub(crate) fn build_root_archive_replacement_history_from_candidate_plan(
    history: &[ResponseItem],
    runtime_spans: &[InstalledCompactSpan],
    root_compact_id: &str,
    initial_context_items: Vec<ResponseItem>,
    root_memory_item: ResponseItem,
    candidate_plan: &CandidateMemPlan,
) -> CodexResult<Vec<ResponseItem>> {
    let mut pi = candidate_plan.pi.clone();
    let artifacts = &candidate_plan.artifacts;
    let note_items = initial_context_items
        .into_iter()
        .enumerate()
        .map(|(index, item)| (format!("initial_context_{index}"), vec![item]))
        .collect::<BTreeMap<_, _>>();
    pi = insert_spine_host_note_segments(
        pi,
        root_compact_id,
        note_items.keys().cloned().collect(),
        NotePlacement::BeforeMem,
    )?;
    let mut mem_items = memory_items_from_runtime_spans(history, runtime_spans)?;
    mem_items.insert(root_compact_id.to_string(), root_memory_item);
    let replacement_history = materialize_spine_host_history(SpineHostMaterializationInput {
        pi: &pi,
        artifacts,
        raw_source: SpineHostRawSource::HostBridge {
            history,
            runtime_spans,
        },
        mem_items: &mem_items,
        note_items: &note_items,
    })?;
    Ok(replacement_history)
}

fn memory_items_from_runtime_spans(
    history: &[ResponseItem],
    runtime_spans: &[InstalledCompactSpan],
) -> CodexResult<BTreeMap<String, ResponseItem>> {
    let mut items = BTreeMap::new();
    let projection = HostBridgeProjection::build(history, runtime_spans)?;
    for span in runtime_spans {
        let item = projection.memory_item_for_span(&span.compact_id)?;
        items.insert(span.compact_id.clone(), item);
    }
    Ok(items)
}

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
