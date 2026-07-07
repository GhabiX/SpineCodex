mod evidence;
mod host_effects;
pub(in crate::spine) mod toolcall;

use super::runtime::SpineError;
use super::runtime::SpineHostEffects;
use super::runtime::SpineSessionState;
pub(crate) use evidence::CompactEvidence;
pub(crate) use evidence::InitEvidence;
pub(crate) use evidence::MessageEvidence;
pub(crate) use host_effects::HostEffects;

pub(crate) fn on_init(
    state: &mut SpineSessionState,
    evidence: InitEvidence<'_>,
) -> Result<HostEffects, SpineError> {
    host_effects_from_runtime_result(state.on_init(evidence.into_runtime()))
}

pub(crate) fn on_non_toolcall_msg(
    state: &mut SpineSessionState,
    evidence: MessageEvidence<'_>,
) -> Result<HostEffects, SpineError> {
    host_effects_from_runtime_result(
        state.observe_non_toolcall_msg_with_host_effects(evidence.into_runtime()),
    )
}

pub(crate) fn on_compact(
    state: &mut SpineSessionState,
    evidence: CompactEvidence<'_>,
) -> Result<HostEffects, SpineError> {
    host_effects_from_runtime_result(
        state.prepare_native_root_compact_from_history_with_checkpoint(evidence.into_runtime()),
    )
}

pub(in crate::spine) fn on_toolcall(
    state: &mut SpineSessionState,
    evidence: toolcall::ToolcallHookEvidence<'_>,
) -> Result<HostEffects, SpineError> {
    host_effects_from_runtime_result(
        state.prepare_completed_toolcall_for_commit(evidence.into_runtime()),
    )
}

fn host_effects_from_runtime_result(
    result: Result<SpineHostEffects, SpineError>,
) -> Result<HostEffects, SpineError> {
    result.map(HostEffects::from_runtime)
}
