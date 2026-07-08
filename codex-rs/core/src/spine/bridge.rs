mod host_effects;
mod replay;
mod toolcall_host_commit;
mod toolcall_lifecycle;
mod toolcall_prepare;
mod toolcall_recording;
mod tree_projection;
mod trim;

pub(crate) use super::runtime::is_non_toolcall_msg;
pub(crate) use replay::ReplayRootCompactBoundary;
pub(crate) use replay::ReplayRuntime;
pub(crate) use toolcall_host_commit::CompletedToolCallHostOutcome;
pub(crate) use toolcall_host_commit::ToolcallHostAttempt;
pub(crate) use toolcall_lifecycle::ToolCallEvidence;
pub(crate) use toolcall_lifecycle::ToolcallPreparedHostCommit;
pub(crate) use toolcall_lifecycle::prepare_completed_toolcall_for_commit;
pub(crate) use toolcall_recording::prepare_grouped_output_recording;
pub(crate) use toolcall_recording::prepare_single_output_recording;
pub(crate) use tree_projection::OpenNodeContextProjection;
pub(crate) use tree_projection::TreeSnapshotProjection;
pub(crate) use trim::TrimRequest;

use crate::context::is_contextual_user_fragment;
use codex_protocol::models::ResponseItem;

pub(crate) fn is_spine_fixed_prefix_item(item: &ResponseItem) -> bool {
    let ResponseItem::Message { role, content, .. } = item else {
        return false;
    };
    match role.as_str() {
        "developer" => true,
        "user" => content.iter().any(is_contextual_user_fragment),
        _ => false,
    }
}
