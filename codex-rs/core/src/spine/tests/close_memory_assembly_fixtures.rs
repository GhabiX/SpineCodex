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
#[path = "close_memory_assembly_source_plan_fixtures.rs"]
mod close_memory_assembly_source_plan_fixtures;
#[path = "close_memory_assembly_source_plan_validator.rs"]
mod close_memory_assembly_source_plan_validator;

pub(super) use close_memory_assembly_message_fixtures::*;
pub(super) use close_memory_assembly_source_fixtures::*;
pub(super) use close_memory_assembly_source_plan_fixtures::*;

pub(super) fn node_id(path: &[u32]) -> NodeId {
    serde_json::from_value(serde_json::json!(path)).expect("node id")
}
