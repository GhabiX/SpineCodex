use super::candidate_mem_plan::CandidateMem;
use super::candidate_mem_plan::CandidateMemCover;
use super::candidate_mem_plan::CandidateMemPlan;
use super::candidate_mem_plan::plan_candidate_mem_cover;
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

#[cfg(test)]
pub(crate) fn build_suffix_replacement_history(
    old_history: &[ResponseItem],
    cut_index: usize,
    fold_end_index: usize,
    ir_items: Vec<ResponseItem>,
) -> Vec<ResponseItem> {
    let mut replacement_history = Vec::with_capacity(
        cut_index + ir_items.len() + old_history.len().saturating_sub(fold_end_index),
    );
    replacement_history.extend_from_slice(&old_history[..cut_index]);
    replacement_history.extend(ir_items);
    replacement_history.extend_from_slice(&old_history[fold_end_index..]);
    replacement_history
}

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
    if !note_items.is_empty() {
        pi = insert_notes_after_mem(pi, compact_id, note_items.keys().cloned().collect())?;
    }
    let mut mem_items = memory_items_from_runtime_spans(old_history, runtime_spans)?;
    mem_items.insert(compact_id.to_string(), memory_item);
    render_pi_bridge_replacement_history(
        old_history,
        runtime_spans,
        &pi,
        artifacts,
        &mem_items,
        &note_items,
    )
}

pub(crate) fn build_root_archive_replacement_history(
    history: &[ResponseItem],
    planned_cut_index: usize,
    fold_end_ordinal: u64,
    initial_context_items: Vec<ResponseItem>,
    root_memory_item: ResponseItem,
    runtime_spans: &[InstalledCompactSpan],
) -> CodexResult<RootArchiveReplacementHistory> {
    build_root_archive_replacement_history_for_compact_id(
        history,
        planned_cut_index,
        fold_end_ordinal,
        initial_context_items,
        root_memory_item,
        runtime_spans,
        "__spine_root_archive_render_pi__",
    )
}

pub(crate) fn build_root_archive_replacement_history_for_compact_id(
    history: &[ResponseItem],
    planned_cut_index: usize,
    fold_end_ordinal: u64,
    initial_context_items: Vec<ResponseItem>,
    root_memory_item: ResponseItem,
    runtime_spans: &[InstalledCompactSpan],
    root_compact_id: &str,
) -> CodexResult<RootArchiveReplacementHistory> {
    let projection = HostBridgeProjection::build(history, runtime_spans)?;
    let (archive_cut_ordinal, archive_cut_index) = projection
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
    let raw_len = projection
        .raw_for_effective_index(history.len())
        .ok_or_else(|| {
            CodexErr::Fatal("spine root archive render(Pi) could not map history end".to_string())
        })?;
    let candidate = CandidateMem::anonymous(
        root_compact_id.to_string(),
        RawSpan {
            start: archive_cut_ordinal,
            end: fold_end_ordinal,
        },
    );
    let CandidateMemCover { pi, artifacts } =
        plan_candidate_mem_cover(raw_len, runtime_spans, &candidate).map_err(pi_render_error)?;
    let candidate_plan = CandidateMemPlan {
        pi,
        artifacts,
        admitted_candidate: true,
        rejected_candidate_reason: None,
    };
    build_root_archive_replacement_history_from_candidate_plan(
        history,
        runtime_spans,
        root_compact_id,
        archive_cut_ordinal,
        archive_cut_index,
        initial_context_items,
        root_memory_item,
        &candidate_plan,
    )
}

pub(crate) fn build_root_archive_replacement_history_from_candidate_plan(
    history: &[ResponseItem],
    runtime_spans: &[InstalledCompactSpan],
    root_compact_id: &str,
    archive_cut_ordinal: u64,
    archive_cut_index: usize,
    initial_context_items: Vec<ResponseItem>,
    root_memory_item: ResponseItem,
    candidate_plan: &CandidateMemPlan,
) -> CodexResult<RootArchiveReplacementHistory> {
    let mut pi = candidate_plan.pi.clone();
    let artifacts = &candidate_plan.artifacts;
    let note_items = initial_context_items
        .into_iter()
        .enumerate()
        .map(|(index, item)| (format!("initial_context_{index}"), vec![item]))
        .collect::<BTreeMap<_, _>>();
    if !note_items.is_empty() {
        pi = insert_notes_before_mem(pi, root_compact_id, note_items.keys().cloned().collect())?;
    }
    let mut mem_items = memory_items_from_runtime_spans(history, runtime_spans)?;
    mem_items.insert(root_compact_id.to_string(), root_memory_item);
    let replacement_history = render_pi_bridge_replacement_history(
        history,
        runtime_spans,
        &pi,
        artifacts,
        &mem_items,
        &note_items,
    )?;
    Ok(RootArchiveReplacementHistory {
        replacement_history,
        archive_cut_ordinal,
        archive_cut_index,
    })
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RootArchiveReplacementHistory {
    pub(crate) replacement_history: Vec<ResponseItem>,
    pub(crate) archive_cut_ordinal: u64,
    pub(crate) archive_cut_index: usize,
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

fn insert_notes_after_mem(
    pi: Vec<Segment>,
    compact_id: &str,
    note_kinds: Vec<String>,
) -> CodexResult<Vec<Segment>> {
    let mut inserted = false;
    let mut result = Vec::with_capacity(pi.len() + note_kinds.len());
    for segment in pi {
        let is_target = matches!(&segment, Segment::Mem { compact_id: id } if id == compact_id);
        result.push(segment);
        if is_target {
            inserted = true;
            result.extend(note_kinds.iter().cloned().map(Segment::note));
        }
    }
    if !inserted {
        return Err(CodexErr::Fatal(format!(
            "render(Pi) could not place notes after Mem {compact_id}"
        )));
    }
    Ok(result)
}

fn insert_notes_before_mem(
    pi: Vec<Segment>,
    compact_id: &str,
    note_kinds: Vec<String>,
) -> CodexResult<Vec<Segment>> {
    let mut inserted = false;
    let mut result = Vec::with_capacity(pi.len() + note_kinds.len());
    for segment in pi {
        if matches!(&segment, Segment::Mem { compact_id: id } if id == compact_id) {
            inserted = true;
            result.extend(note_kinds.iter().cloned().map(Segment::note));
        }
        result.push(segment);
    }
    if !inserted {
        return Err(CodexErr::Fatal(format!(
            "render(Pi) could not place notes before Mem {compact_id}"
        )));
    }
    Ok(result)
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
