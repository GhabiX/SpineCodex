use super::*;
use crate::spine::NodeId;

#[path = "close_memory_assembly_close_like_filter.rs"]
mod close_memory_assembly_close_like_filter;
#[path = "close_memory_assembly_exact_evidence.rs"]
mod close_memory_assembly_exact_evidence;
#[path = "close_memory_assembly_node_memory.rs"]
mod close_memory_assembly_node_memory;
#[path = "close_memory_assembly_source_plan_validator.rs"]
mod close_memory_assembly_source_plan_validator;

pub(super) fn node_id(path: &[u32]) -> NodeId {
    serde_json::from_value(serde_json::json!(path)).expect("node id")
}

pub(super) fn assistant_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: text.to_string(),
        }],
        phase: None,
    }
}

pub(super) fn user_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
    }
}

pub(super) fn source_entry(
    context_index: usize,
    source_ordinal: usize,
    item: ResponseItem,
    from_user: bool,
) -> crate::spine::SpineCompactSourcePlanEntry {
    source_entry_with_user_anchor(
        context_index,
        source_ordinal,
        item,
        from_user,
        from_user.then_some(1),
    )
}

pub(super) fn source_entry_with_user_anchor(
    context_index: usize,
    source_ordinal: usize,
    item: ResponseItem,
    from_user: bool,
    user_anchor: Option<u64>,
) -> crate::spine::SpineCompactSourcePlanEntry {
    crate::spine::SpineCompactSourcePlanEntry {
        context_index,
        source_ordinal,
        source_hash: format!("hash-{source_ordinal}"),
        kind: SpineCompactSourceEntryKind::RawResponseItem {
            item,
            raw_ordinal: u64::try_from(context_index).expect("context index fits u64"),
            from_user,
            user_anchor,
        },
    }
}

pub(super) fn child_memory_entry(
    context_index: usize,
    source_ordinal: usize,
    body: &str,
) -> crate::spine::SpineCompactSourcePlanEntry {
    crate::spine::SpineCompactSourcePlanEntry {
        context_index,
        source_ordinal,
        source_hash: format!("child-hash-{source_ordinal}"),
        kind: SpineCompactSourceEntryKind::ChildMemory {
            node_id: node_id(&[1, 1, 1]),
            compact_id: "mem-1-1-1".to_string(),
            source_raw_range: 2..3,
            body: body.to_string(),
            body_hash: "body-hash".to_string(),
        },
    }
}

pub(super) fn source_plan(
    entries: Vec<crate::spine::SpineCompactSourcePlanEntry>,
) -> SpineCompactSourcePlan {
    SpineCompactSourcePlan {
        node_id: node_id(&[1, 1]),
        source_context_range: 2..2 + entries.len(),
        source_raw_range: 2..2 + u64::try_from(entries.len()).expect("entries len fits u64"),
        entries,
    }
}

pub(super) fn source_plan_with_context_range(
    source_context_range: std::ops::Range<usize>,
    entries: Vec<crate::spine::SpineCompactSourcePlanEntry>,
) -> SpineCompactSourcePlan {
    SpineCompactSourcePlan {
        node_id: node_id(&[1, 1]),
        source_raw_range: u64::try_from(source_context_range.start).expect("range start fits u64")
            ..u64::try_from(source_context_range.end).expect("range end fits u64"),
        source_context_range,
        entries,
    }
}
