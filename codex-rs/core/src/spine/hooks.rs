use super::runtime::SpineError;
use super::runtime::SpineHostEffects;
use super::runtime::SpineInitEvidence;
use super::runtime::SpineMessageEvidence;
use super::runtime::SpineMessageHostOutcome;
use super::runtime::SpineSessionState;
use std::path::Path;

pub(crate) fn on_init(
    state: &mut SpineSessionState,
    evidence: SpineInitEvidence<'_>,
) -> Result<SpineHostEffects, SpineError> {
    state.on_init(evidence)
}

pub(crate) fn on_non_toolcall_msg(
    state: &mut SpineSessionState,
    rollout_path: &Path,
    evidence: SpineMessageEvidence<'_>,
) -> Result<SpineMessageHostOutcome, SpineError> {
    state.observe_non_toolcall_msg_with_host_effects(rollout_path, evidence)
}
