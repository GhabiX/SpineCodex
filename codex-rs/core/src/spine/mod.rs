mod archive;
mod checkpoint;
mod compact_checkpoint;
mod instructions;
mod io;
mod lexer;
mod model;
mod parse_stack;
mod render;
mod runtime;
mod store;
mod trimmer;
mod user_message_projection;

pub(crate) use model::NodeId;
pub(crate) use model::ToolCallSegmentKind;
pub(crate) use runtime::CompletedToolCall;
pub(crate) use runtime::CompletedToolCallSegment;
#[cfg(test)]
pub(crate) use runtime::IntoSpineNodeMemory;
pub(crate) use runtime::LiveRootCompact;
pub(crate) use runtime::SPINE_CONTROL_MULTI_CALL_REJECTION_PREFIX;
pub(crate) use runtime::SPINE_NAMESPACE;
pub(crate) use runtime::SPINE_TOOL_CLOSE;
pub(crate) use runtime::SPINE_TOOL_NEXT;
pub(crate) use runtime::SPINE_TOOL_OPEN;
pub(crate) use runtime::SPINE_TOOL_TREE;
pub(crate) use runtime::SPINE_TOOL_TRIM;
pub(crate) use runtime::SpineCloseMemoryAssembly;
pub(crate) use runtime::SpineCommitKind;
pub(crate) use runtime::SpineCompactSourceEntryKind;
pub(crate) use runtime::SpineCompactSourcePlan;
#[cfg(test)]
pub(crate) use runtime::SpineCompactSourcePlanEntry;
pub(crate) use runtime::SpineCompletedToolCallHostOutcome;
pub(crate) use runtime::SpineCompletedToolCallOutputEvidence;
pub(crate) use runtime::SpineError;
pub(crate) use runtime::SpineHostEffects;
pub(crate) use runtime::SpineInitEvidence;
pub(crate) use runtime::SpineMessageEvidence;
pub(crate) use runtime::SpineMessageHostOutcome;
pub(crate) use runtime::SpineObservedContextItem;
pub(crate) use runtime::SpineOpenNodeContextProjection;
pub(crate) use runtime::SpinePendingCommit;
pub(crate) use runtime::SpinePreparedCommit;
pub(crate) use runtime::SpinePreparedRootCompact;
pub(crate) use runtime::SpineRootCompactHostInstall;
pub(crate) use runtime::SpineRootCompactHostOutcome;
pub(crate) use runtime::SpineRootCompactHostPublish;
#[cfg(test)]
pub(crate) use runtime::SpineRootCompactResult;
pub(crate) use runtime::SpineRootCompactTokenMetadata;
pub(crate) use runtime::SpineRuntime;
pub(crate) use runtime::SpineSessionState;
pub(crate) use runtime::SpineTokenBaselines;
pub(crate) use runtime::SpineToolCallEvidence;
#[cfg(test)]
pub(crate) use runtime::SpineToolOutputRecording;
pub(crate) use runtime::SpineToolcallCommitEvidence;
pub(crate) use runtime::SpineToolcallCommitHostLoop;
pub(crate) use runtime::SpineToolcallCommitProviderInputTokens;
pub(crate) use runtime::SpineTrimOutcome;
pub(crate) use runtime::is_non_toolcall_msg;
pub(crate) use runtime::is_real_user_message;
#[cfg(test)]
pub(crate) use runtime::is_spine_close_like_tool_name;
pub(crate) use runtime::is_spine_context_observation_fixed_prefix_item;
pub(crate) use runtime::is_user_message;
pub use store::SpineCloneBoundary;
pub(crate) use store::SpineStore;
pub(crate) use user_message_projection::user_message_memory_body;

pub(crate) use instructions::append_spine_view_instructions;

const CHECKPOINT_VERSION: u32 = 1;
