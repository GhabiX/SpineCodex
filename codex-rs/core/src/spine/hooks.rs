mod evidence;
mod host_effects;
mod runtime_facade;
mod toolcall;
mod toolcall_host_commit;
mod toolcall_recording;
mod tree_projection;

use super::runtime::SpineError;
use super::runtime::SpineSessionState;
pub(crate) use evidence::CompactEvidence;
pub(crate) use evidence::InitEvidence;
pub(crate) use evidence::MessageEvidence;
pub(crate) use runtime_facade::LifecycleRuntime;
pub(crate) use runtime_facade::MessageRuntime;
pub(crate) use runtime_facade::ReplayRuntime;
#[cfg(test)]
pub(crate) use runtime_facade::TestRuntime;
pub(crate) use runtime_facade::TrimRuntime;
pub(crate) use toolcall::CompletedToolCallOutputEvidence;
pub(crate) use toolcall::ToolCallEvidence;
pub(crate) use toolcall::ToolcallHookEvidence;
pub(crate) use toolcall_host_commit::CompletedToolCallHostOutcome;
pub(crate) use toolcall_host_commit::ToolcallHostAttempt;
pub(crate) use toolcall_host_commit::ToolcallHostCommitInput;
pub(crate) use toolcall_recording::ToolcallOutputRecordingPlan;
pub(crate) use toolcall_recording::ToolcallOutputRecordingRequest;
pub(crate) use tree_projection::OpenNodeContextProjection;
pub(crate) use tree_projection::TreeSnapshotProjection;

pub(crate) struct HostEffects {
    inner: super::runtime::SpineHostEffects,
}

pub(crate) struct TreeHostUpdates {
    inner: super::runtime::SpineTreeHostUpdates,
}

pub(crate) struct HistoryHostEffect {
    inner: super::runtime::SpineHostEffect,
}

pub(crate) fn on_init(
    state: &mut SpineSessionState,
    evidence: InitEvidence<'_>,
) -> Result<HostEffects, SpineError> {
    state
        .on_init(super::runtime::SpineInitEvidence {
            rollout_path: evidence.rollout_path,
        })
        .map(HostEffects::from_runtime)
}

pub(crate) fn on_non_toolcall_msg(
    state: &mut SpineSessionState,
    evidence: MessageEvidence<'_>,
) -> Result<HostEffects, SpineError> {
    state
        .observe_non_toolcall_msg_with_host_effects(super::runtime::SpineMessageEvidence {
            rollout_path: evidence.rollout_path,
            raw_ordinal: evidence.raw_ordinal,
            context_index: evidence.context_index,
            item: evidence.item,
            raw_items: evidence.raw_items,
        })
        .map(HostEffects::from_runtime)
}

pub(crate) fn on_compact(
    state: &mut SpineSessionState,
    evidence: CompactEvidence<'_>,
) -> Result<HostEffects, SpineError> {
    state
        .prepare_native_root_compact_from_history_with_checkpoint(
            super::runtime::SpineCompactEvidence {
                rollout_path: evidence.rollout_path,
                compacted_history: evidence.compacted_history,
                raw_items: evidence.raw_items,
                close_provider_input_tokens: evidence.close_provider_input_tokens,
            },
        )
        .map(HostEffects::from_runtime)
}

pub(crate) fn on_toolcall(
    state: &mut SpineSessionState,
    evidence: ToolcallHookEvidence<'_>,
) -> Result<HostEffects, SpineError> {
    state
        .prepare_completed_toolcall_for_commit(super::runtime::SpineToolcallHookEvidence {
            completed_output: &evidence.completed_output.inner,
            output_raw_ordinals: evidence.output_raw_ordinals,
            output_context_start: evidence.output_context_start,
            raw_items: evidence.raw_items,
            current_turn_provider_input_tokens: evidence.current_turn_provider_input_tokens,
            tool_resp_already_recorded: evidence.tool_resp_already_recorded,
            recorded_inside_reduce: evidence.recorded_inside_reduce,
        })
        .map(HostEffects::from_runtime)
}
