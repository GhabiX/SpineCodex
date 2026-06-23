use super::runtime::SpineCompactEvidence;
use super::runtime::SpineError;
use super::runtime::SpineHostEffects;
use super::runtime::SpineInitEvidence;
use super::runtime::SpineMessageEvidence;
use super::runtime::SpineSessionState;
use super::runtime::SpineToolcallHookEvidence;

pub(crate) fn on_init(
    state: &mut SpineSessionState,
    evidence: SpineInitEvidence<'_>,
) -> Result<SpineHostEffects, SpineError> {
    state.on_init(evidence)
}

pub(crate) fn on_non_toolcall_msg(
    state: &mut SpineSessionState,
    evidence: SpineMessageEvidence<'_>,
) -> Result<SpineHostEffects, SpineError> {
    state.observe_non_toolcall_msg_with_host_effects(evidence)
}

pub(crate) fn on_compact(
    state: &mut SpineSessionState,
    evidence: SpineCompactEvidence<'_>,
) -> Result<SpineHostEffects, SpineError> {
    state.prepare_native_root_compact_from_history_with_checkpoint(evidence)
}

pub(crate) fn on_toolcall(
    state: &mut SpineSessionState,
    evidence: SpineToolcallHookEvidence<'_>,
) -> Result<SpineHostEffects, SpineError> {
    state.prepare_completed_toolcall_for_commit(evidence)
}
