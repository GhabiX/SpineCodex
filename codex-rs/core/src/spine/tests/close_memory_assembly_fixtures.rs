use super::*;
use crate::spine::NodeId;

#[path = "close_memory_assembly_close_like_filter.rs"]
mod close_memory_assembly_close_like_filter;
#[path = "close_memory_assembly_exact_evidence.rs"]
mod close_memory_assembly_exact_evidence;
#[path = "close_memory_assembly_message_fixtures.rs"]
mod close_memory_assembly_message_fixtures;
#[path = "close_memory_assembly_node_memory.rs"]
mod close_memory_assembly_node_memory;
#[path = "close_memory_assembly_source_fixtures.rs"]
mod close_memory_assembly_source_fixtures;
#[path = "close_memory_assembly_source_plan_validator.rs"]
mod close_memory_assembly_source_plan_validator;

pub(super) use close_memory_assembly_message_fixtures::*;
pub(super) use close_memory_assembly_source_fixtures::*;

pub(super) fn node_id(path: &[u32]) -> NodeId {
    serde_json::from_value(serde_json::json!(path)).expect("node id")
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
