use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use std::collections::BTreeSet;

use super::CompletedToolCall;
use super::CompletedToolCallSegment;
use super::SPINE_NAMESPACE;
use super::SPINE_TOOL_CLOSE;
use super::SPINE_TOOL_NEXT;
use super::SPINE_TOOL_OPEN;
use super::SPINE_TOOL_TREE;
use super::SpineCompactSourceEntryKind;
use super::SpineCompactSourcePlanEntry;
use super::SpineError;
use crate::spine::io::hash_response_items;
use crate::spine::model::COMMIT_MARKER_VERSION;
use crate::spine::model::MemRecord;
use crate::spine::model::SpineCommitKindMarker;
use crate::spine::model::SpineCommitMarker;
use crate::spine::model::SpineCommitMemoryRef;
use crate::spine::model::SpineLedgerEvent;
use crate::spine::render::VisibleItemSource;
use crate::spine::render::memory_response_item;
use crate::spine::render::read_memory_ref_body;

pub(super) fn mark_raw_covered(covered: &mut [bool], raw_ordinal: u64) -> Result<(), SpineError> {
    let index = usize::try_from(raw_ordinal)
        .map_err(|_| SpineError::InvalidEvent("raw ordinal overflow".to_string()))?;
    if let Some(slot) = covered.get_mut(index) {
        *slot = true;
    }
    Ok(())
}

pub(super) fn mark_raw_prefix_covered(
    covered: &mut [bool],
    boundary: u64,
) -> Result<(), SpineError> {
    let boundary = usize::try_from(boundary)
        .map_err(|_| SpineError::InvalidEvent("raw boundary overflow".to_string()))?;
    for slot in covered.iter_mut().take(boundary) {
        *slot = true;
    }
    Ok(())
}

#[derive(Clone, Copy)]
pub(super) enum ToolRawItemKind {
    SpineControlRequest,
    SpineTreeRequest,
    Request,
    Response,
}

pub(super) fn completed_toolcall_first_segment(
    toolcall: &CompletedToolCall,
) -> Result<&CompletedToolCallSegment, SpineError> {
    toolcall.segments.first().ok_or_else(|| {
        SpineError::InvalidEvent("completed toolcall must contain at least one segment".to_string())
    })
}

pub(super) fn raw_item_requires_spine_coverage(
    item: &ResponseItem,
    _spine_control_call_ids: &BTreeSet<String>,
    _spine_tree_call_ids: &BTreeSet<String>,
    completed_tool_call_ids: &BTreeSet<String>,
) -> bool {
    match item {
        ResponseItem::FunctionCall {
            call_id,
            namespace: Some(namespace),
            name,
            ..
        } if namespace == SPINE_NAMESPACE && is_spine_parser_control_tool_name(name) => {
            completed_tool_call_ids.contains(call_id)
        }
        ResponseItem::FunctionCall {
            call_id,
            namespace: Some(namespace),
            name,
            ..
        } if namespace == SPINE_NAMESPACE && name == SPINE_TOOL_TREE => {
            completed_tool_call_ids.contains(call_id)
        }
        ResponseItem::Other | ResponseItem::CompactionTrigger => false,
        item => {
            if let Some(call_id) = tool_response_call_id(item) {
                return completed_tool_call_ids.contains(call_id);
            }
            if let Some(call_id) = tool_request_call_id(item) {
                return completed_tool_call_ids.contains(call_id);
            }
            true
        }
    }
}

pub(super) fn tool_request_call_id(item: &ResponseItem) -> Option<&str> {
    match item {
        ResponseItem::FunctionCall { call_id, .. }
        | ResponseItem::CustomToolCall { call_id, .. } => Some(call_id.as_str()),
        ResponseItem::ToolSearchCall {
            call_id: Some(call_id),
            ..
        } => Some(call_id.as_str()),
        _ => None,
    }
}

pub(super) fn tool_response_call_id(item: &ResponseItem) -> Option<&str> {
    match item {
        ResponseItem::FunctionCallOutput { call_id, .. }
        | ResponseItem::CustomToolCallOutput { call_id, .. } => Some(call_id.as_str()),
        ResponseItem::ToolSearchOutput {
            call_id: Some(call_id),
            ..
        } => Some(call_id.as_str()),
        _ => None,
    }
}

pub(crate) fn is_non_toolcall_msg(item: &ResponseItem) -> bool {
    tool_request_call_id(item).is_none()
        && tool_response_call_id(item).is_none()
        && !matches!(
            item,
            ResponseItem::ToolSearchOutput { call_id: None, .. }
                | ResponseItem::ToolSearchCall { call_id: None, .. }
        )
}

impl SpineCompactSourcePlanEntry {
    pub(crate) fn visible_response_item(&self) -> ResponseItem {
        match &self.kind {
            SpineCompactSourceEntryKind::RawResponseItem { item, .. } => item.clone(),
            SpineCompactSourceEntryKind::ChildMemory { body, .. } => memory_response_item(body),
        }
    }
}

pub(super) fn collect_source_plan_entries_from_visible_refs(
    visible_refs: &[crate::spine::render::VisibleItemRef],
    raw_context_items: &[ResponseItem],
) -> Result<Vec<SpineCompactSourcePlanEntry>, SpineError> {
    let mut entries = Vec::with_capacity(visible_refs.len());
    for visible_ref in visible_refs {
        match &visible_ref.source {
            VisibleItemSource::RawResponseItem {
                raw_ordinal,
                from_user,
                user_anchor,
            } => collect_source_plan_entry_from_response_item(
                *raw_ordinal,
                visible_ref.context_index,
                *from_user,
                *user_anchor,
                raw_context_items,
                &mut entries,
            )?,
            VisibleItemSource::ToolCallSegment { raw_ordinal, kind } => {
                let _ = kind;
                collect_source_plan_entry_from_response_item(
                    *raw_ordinal,
                    visible_ref.context_index,
                    false,
                    None,
                    raw_context_items,
                    &mut entries,
                )?;
            }
            VisibleItemSource::MemoryRef { memory, .. } => {
                let source_ordinal = entries.len();
                let body = read_memory_ref_body(memory)?;
                let visible_item = memory_response_item(&body);
                let source_hash = hash_response_items(std::slice::from_ref(&visible_item))?;
                entries.push(SpineCompactSourcePlanEntry {
                    context_index: visible_ref.context_index,
                    source_ordinal,
                    source_hash,
                    kind: SpineCompactSourceEntryKind::ChildMemory {
                        node_id: memory.node_id.clone(),
                        compact_id: memory.compact_id.clone(),
                        source_raw_range: memory.source_raw_range.clone(),
                        body,
                        body_hash: memory.body_hash.clone(),
                    },
                });
            }
            VisibleItemSource::MemorySeg { memory_id, .. } => {
                return Err(SpineError::CompactFailure(format!(
                    "spine.close source plan cannot trust SegRef::Memory {memory_id} without MemoryRef body_hash provenance"
                )));
            }
        }
    }
    Ok(entries)
}

fn collect_source_plan_entry_from_response_item(
    raw_ordinal: u64,
    context_index: usize,
    from_user: bool,
    user_anchor: Option<u64>,
    raw_context_items: &[ResponseItem],
    entries: &mut Vec<SpineCompactSourcePlanEntry>,
) -> Result<(), SpineError> {
    let source_ordinal = entries.len();
    let item = raw_context_items
        .get(context_index)
        .cloned()
        .ok_or_else(|| {
            SpineError::CompactFailure(format!(
                "spine.close source plan raw item context_index {context_index} exceeds host history length {}",
                raw_context_items.len()
            ))
        })?;
    let source_hash = hash_response_items(std::slice::from_ref(&item))?;
    entries.push(SpineCompactSourcePlanEntry {
        context_index,
        source_ordinal,
        source_hash,
        kind: SpineCompactSourceEntryKind::RawResponseItem {
            item,
            raw_ordinal,
            from_user,
            user_anchor,
        },
    });
    Ok(())
}

pub(super) fn validate_model_node_memory(memory: &str) -> Result<(), SpineError> {
    if memory.trim().is_empty() {
        return Err(SpineError::ToolUse(
            "spine.close/next memory must not be empty".to_string(),
        ));
    }
    Ok(())
}

pub(super) fn user_anchor_refs_in_memory(memory: &str) -> Result<BTreeSet<u64>, SpineError> {
    let bytes = memory.as_bytes();
    let mut refs = BTreeSet::new();
    let mut offset = 0usize;
    while let Some(relative_start) = memory[offset..].find("[U") {
        let start = offset
            .checked_add(relative_start)
            .ok_or_else(|| SpineError::InvalidEvent("user anchor scan overflow".to_string()))?;
        let digits_start = start
            .checked_add(2)
            .ok_or_else(|| SpineError::InvalidEvent("user anchor scan overflow".to_string()))?;
        let mut digits_end = digits_start;
        while digits_end < bytes.len() && bytes[digits_end].is_ascii_digit() {
            digits_end += 1;
        }
        if digits_end > digits_start && bytes.get(digits_end) == Some(&b']') {
            let anchor = memory[digits_start..digits_end]
                .parse::<u64>()
                .map_err(|_| {
                    SpineError::ToolUse(
                        "spine.close/next memory contains invalid user anchor".to_string(),
                    )
                })?;
            refs.insert(anchor);
            offset = digits_end
                .checked_add(1)
                .ok_or_else(|| SpineError::InvalidEvent("user anchor scan overflow".to_string()))?;
        } else {
            offset = start
                .checked_add(2)
                .ok_or_else(|| SpineError::InvalidEvent("user anchor scan overflow".to_string()))?;
        }
    }
    Ok(refs)
}

pub(super) fn validate_source_plan_context_index(
    source_ordinal: usize,
    context_index: usize,
    suffix_start: usize,
    source_context_end: usize,
    previous_context_index: &mut Option<usize>,
) -> Result<(), SpineError> {
    if context_index < suffix_start {
        return Err(SpineError::CompactFailure(format!(
            "spine.close source plan entry ordinal {source_ordinal} context_index {context_index} precedes suffix start {suffix_start}"
        )));
    }
    if context_index >= source_context_end {
        return Err(SpineError::CompactFailure(format!(
            "spine.close source plan entry ordinal {source_ordinal} context_index {context_index} is outside source context range [{suffix_start}..{source_context_end})"
        )));
    }
    if let Some(previous) = *previous_context_index {
        if context_index <= previous {
            return Err(SpineError::CompactFailure(format!(
                "spine.close source plan entry ordinal {source_ordinal} context_index {context_index} is not strictly after previous context_index {previous}"
            )));
        }
    }
    *previous_context_index = Some(context_index);
    Ok(())
}

pub(super) fn close_event_boundary(event: &SpineLedgerEvent) -> Result<u64, SpineError> {
    match event {
        SpineLedgerEvent::Close { boundary, .. } => Ok(*boundary),
        _ => Err(SpineError::Invariant(
            "close commit marker requested for non-close event".to_string(),
        )),
    }
}

pub(super) fn close_commit_marker(
    seq: u64,
    mem: &MemRecord,
    kind: SpineCommitKindMarker,
    raw_boundary: u64,
    width: u64,
) -> Result<SpineCommitMarker, SpineError> {
    if kind == SpineCommitKindMarker::RootCompact {
        return Err(SpineError::Invariant(
            "root compact marker requested from close marker builder".to_string(),
        ));
    }
    Ok(SpineCommitMarker {
        version: COMMIT_MARKER_VERSION,
        op_id: format!("{}:{}", commit_marker_kind_label(kind), mem.compact_id),
        kind,
        token_seq_start: seq,
        token_seq_end: seq.checked_add(width).ok_or_else(|| {
            SpineError::InvalidEvent("Spine commit marker token seq overflow".to_string())
        })?,
        raw_boundary,
        raw_live_hash: None,
        memory_refs: vec![commit_memory_ref(mem)],
    })
}

pub(super) fn root_compact_commit_marker(
    seq: u64,
    mem: &MemRecord,
) -> Result<SpineCommitMarker, SpineError> {
    Ok(SpineCommitMarker {
        version: COMMIT_MARKER_VERSION,
        op_id: format!("root_compact:{}", mem.compact_id),
        kind: SpineCommitKindMarker::RootCompact,
        token_seq_start: seq,
        token_seq_end: seq.checked_add(1).ok_or_else(|| {
            SpineError::InvalidEvent("Spine commit marker token seq overflow".to_string())
        })?,
        raw_boundary: mem.raw_end,
        raw_live_hash: mem.raw_live_hash.clone(),
        memory_refs: vec![commit_memory_ref(mem)],
    })
}

fn commit_marker_kind_label(kind: SpineCommitKindMarker) -> &'static str {
    match kind {
        SpineCommitKindMarker::Close => "close",
        SpineCommitKindMarker::CloseThenOpen => "close_then_open",
        SpineCommitKindMarker::RootCompact => "root_compact",
    }
}

fn commit_memory_ref(mem: &MemRecord) -> SpineCommitMemoryRef {
    SpineCommitMemoryRef {
        compact_id: mem.compact_id.clone(),
        kind: mem.kind,
        node: mem.node.clone(),
        raw_start: mem.raw_start,
        raw_end: mem.raw_end,
        context_start: mem.context_start,
        context_end: mem.context_end,
        raw_live_hash: mem.raw_live_hash.clone(),
        body_path: mem.body_path.clone(),
        body_hash: mem.body_hash.clone(),
    }
}

pub(super) fn mem_record_matches(existing: &MemRecord, expected: &MemRecord) -> bool {
    existing.compact_id == expected.compact_id
        && existing.kind == expected.kind
        && existing.node == expected.node
        && existing.raw_start == expected.raw_start
        && existing.raw_end == expected.raw_end
        && existing.context_start == expected.context_start
        && existing.context_end == expected.context_end
        && existing.raw_live_hash == expected.raw_live_hash
        && existing.open_input_tokens == expected.open_input_tokens
        && existing.close_input_tokens == expected.close_input_tokens
        && existing.open_context_tokens == expected.open_context_tokens
        && existing.close_context_tokens == expected.close_context_tokens
        && existing.closed_source_suffix_tokens == expected.closed_source_suffix_tokens
        && existing.closed_memory_context_tokens == expected.closed_memory_context_tokens
        && existing.open_context_source == expected.open_context_source
        && existing.memory_output_tokens == expected.memory_output_tokens
        && existing.body_path == expected.body_path
        && existing.body_hash == expected.body_hash
}

pub(crate) fn is_user_message(item: &ResponseItem) -> bool {
    matches!(item, ResponseItem::Message { role, .. } if role == "user")
}

pub(crate) fn is_real_user_message(item: &ResponseItem) -> bool {
    let ResponseItem::Message { role, content, .. } = item else {
        return false;
    };
    role == "user"
        && !content
            .iter()
            .any(crate::context::is_contextual_user_fragment)
        && !content.iter().any(is_spine_memory_fragment)
}

fn is_spine_memory_fragment(content_item: &ContentItem) -> bool {
    let ContentItem::InputText { text } = content_item else {
        return false;
    };
    text.trim_start().starts_with("<spine_memory>")
}

pub(super) fn is_spine_parser_control_tool_name(name: &str) -> bool {
    matches!(name, SPINE_TOOL_OPEN | SPINE_TOOL_CLOSE | SPINE_TOOL_NEXT)
}

#[cfg(test)]
pub(crate) fn is_spine_close_like_tool_name(name: &str) -> bool {
    matches!(name, SPINE_TOOL_CLOSE | SPINE_TOOL_NEXT)
}
