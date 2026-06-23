use super::runtime::SpineCompactEvidence;
use super::runtime::SpineError;
use super::runtime::SpineHostEffects;
use super::runtime::SpineInitEvidence;
use super::runtime::SpineMessageEvidence;
use super::runtime::SpineMessageHostOutcome;
use super::runtime::SpineRootCompactHostPublish;
use super::runtime::SpineSessionState;
use codex_protocol::models::ResponseItem;
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

pub(crate) fn on_compact(
    state: &mut SpineSessionState,
    rollout_path: &Path,
    raw_items: &[Option<ResponseItem>],
    close_provider_input_tokens: Option<i64>,
    evidence: SpineCompactEvidence<'_>,
) -> Result<Option<SpineRootCompactHostPublish>, SpineError> {
    state.prepare_native_root_compact_from_history_with_checkpoint(
        rollout_path,
        evidence.compacted_history,
        raw_items,
        close_provider_input_tokens,
    )
}
