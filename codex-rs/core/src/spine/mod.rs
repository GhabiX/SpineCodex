pub(crate) mod adapter;
mod archive;
pub(crate) mod bridge;
mod checkpoint;
mod compact_checkpoint;
pub(crate) mod hooks;
mod instructions;
mod io;
mod lexer;
mod model;
mod parse_stack;
mod parser;
mod render;
mod runtime;
mod store;
mod trimmer;
mod user_message_projection;

pub(crate) use model::NodeId;
#[cfg(test)]
pub(crate) use model::TOOL_RESPONSE_TRIM_THRESHOLD_BYTES;
pub(crate) use model::TrimBodyUpdate;
pub(crate) use model::TrimResponseKind;
#[cfg(test)]
pub(crate) use runtime::IntoSpineNodeMemory;
pub(crate) use runtime::SPINE_NAMESPACE;
pub(crate) use runtime::SPINE_TOOL_CLOSE;
pub(crate) use runtime::SPINE_TOOL_NEXT;
pub(crate) use runtime::SPINE_TOOL_OPEN;
pub(crate) use runtime::SPINE_TOOL_TREE;
pub(crate) use runtime::SPINE_TOOL_TRIM;
#[cfg(test)]
pub(crate) use runtime::SpineCloseMemoryAssembly;
#[cfg(test)]
pub(crate) use runtime::SpineCompactSourceEntryKind;
#[cfg(test)]
pub(crate) use runtime::SpineCompactSourcePlan;
#[cfg(test)]
pub(crate) use runtime::SpineCompactSourcePlanEntry;
pub(crate) use runtime::SpineCurrentTrimTarget;
pub(crate) use runtime::SpineError;
#[cfg(test)]
pub(crate) use runtime::SpineRootCompactHostInstall;
#[cfg(test)]
pub(crate) use runtime::SpineRootCompactResult;
#[cfg(test)]
pub(crate) use runtime::SpineRuntime;
pub(crate) use runtime::SpineSessionState;
#[cfg(test)]
pub(crate) use runtime::SpineToolOutputRecording;
pub(crate) use runtime::SpineTrimOutcome;
pub(crate) use runtime::SpinetreeMemoryProjectionConfig;
pub(crate) use runtime::conflicting_spine_control_rejection_reason;
#[cfg(test)]
pub(crate) use runtime::is_spine_close_like_tool_name;
pub(crate) use runtime::is_spine_context_observation_fixed_prefix_item;
pub(crate) use runtime::is_spine_parser_control_tool;
pub(crate) use runtime::spine_mutable_context_index_for_full_history_boundary;
pub(crate) use runtime::spine_mutable_context_index_for_full_history_index;
pub(crate) use runtime::spine_tool_use_failed_message;
#[cfg(test)]
pub(crate) use store::SnapshotTurnState;
pub use store::SpineCloneBoundary;
pub(crate) use store::SpineStore;
#[cfg(test)]
pub(crate) use user_message_projection::user_message_memory_body;

pub(crate) use instructions::append_spine_view_instructions;

pub fn has_spine_store_for_rollout(path: &std::path::Path) -> std::io::Result<bool> {
    SpineStore::has_for_rollout(path).map_err(std::io::Error::other)
}

const CHECKPOINT_VERSION: u32 = 1;
