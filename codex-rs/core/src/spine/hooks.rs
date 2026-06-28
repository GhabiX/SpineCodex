mod evidence;
mod host_effects;
pub(in crate::spine) mod toolcall;

use super::runtime::SpineError;
use super::runtime::SpineSessionState;
pub(crate) use evidence::CompactEvidence;
pub(crate) use evidence::InitEvidence;
pub(crate) use evidence::MessageEvidence;
pub(crate) use host_effects::HostEffects;
pub(crate) use toolcall::ToolCallEvidence;

pub(crate) fn on_init(
    state: &mut SpineSessionState,
    evidence: InitEvidence<'_>,
) -> Result<HostEffects, SpineError> {
    state
        .on_init(evidence.into_runtime())
        .map(HostEffects::from_runtime)
}

pub(crate) fn on_non_toolcall_msg(
    state: &mut SpineSessionState,
    evidence: MessageEvidence<'_>,
) -> Result<HostEffects, SpineError> {
    state
        .observe_non_toolcall_msg_with_host_effects(evidence.into_runtime())
        .map(HostEffects::from_runtime)
}

pub(crate) fn on_compact(
    state: &mut SpineSessionState,
    evidence: CompactEvidence<'_>,
) -> Result<HostEffects, SpineError> {
    state
        .prepare_native_root_compact_from_history_with_checkpoint(evidence.into_runtime())
        .map(HostEffects::from_runtime)
}

pub(in crate::spine) fn on_toolcall(
    state: &mut SpineSessionState,
    evidence: toolcall::ToolcallHookEvidence<'_>,
) -> Result<HostEffects, SpineError> {
    state
        .prepare_completed_toolcall_for_commit(evidence.into_runtime())
        .map(HostEffects::from_runtime)
}
