use super::runtime::SpineError;
use super::runtime::SpineHostEffects;
use super::runtime::SpineInitEvidence;
use super::runtime::SpineSessionState;

pub(crate) fn on_init(
    state: &mut SpineSessionState,
    evidence: SpineInitEvidence<'_>,
) -> Result<SpineHostEffects, SpineError> {
    state.on_init(evidence)
}
