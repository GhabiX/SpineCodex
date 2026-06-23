use super::runtime::SpineError;
use super::runtime::SpineSessionState;

pub(crate) use super::runtime::SpineCompactEvidence as CompactEvidence;
pub(crate) use super::runtime::SpineHostEffects as HostEffects;
pub(crate) use super::runtime::SpineInitEvidence as InitEvidence;
pub(crate) use super::runtime::SpineMessageEvidence as MessageEvidence;
pub(crate) use super::runtime::SpineToolCallEvidence as ToolCallEvidence;
pub(crate) use super::runtime::SpineToolcallHookEvidence as ToolcallHookEvidence;

pub(crate) fn on_init(
    state: &mut SpineSessionState,
    evidence: InitEvidence<'_>,
) -> Result<HostEffects, SpineError> {
    state.on_init(evidence)
}

pub(crate) fn on_non_toolcall_msg(
    state: &mut SpineSessionState,
    evidence: MessageEvidence<'_>,
) -> Result<HostEffects, SpineError> {
    state.observe_non_toolcall_msg_with_host_effects(evidence)
}

pub(crate) fn on_compact(
    state: &mut SpineSessionState,
    evidence: CompactEvidence<'_>,
) -> Result<HostEffects, SpineError> {
    state.prepare_native_root_compact_from_history_with_checkpoint(evidence)
}

pub(crate) fn on_toolcall(
    state: &mut SpineSessionState,
    evidence: ToolcallHookEvidence<'_>,
) -> Result<HostEffects, SpineError> {
    state.prepare_completed_toolcall_for_commit(evidence)
}
