use super::runtime::SpineCompactEvidence;
use super::runtime::SpineError;
use super::runtime::SpineHostEffects;
use super::runtime::SpineInitEvidence;
use super::runtime::SpineMessageEvidence;
use super::runtime::SpineSessionState;
use super::runtime::SpineToolcallCommitEvidence;
use codex_protocol::models::ResponseItem;

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
    evidence: &SpineToolcallCommitEvidence,
    raw_items: &[Option<ResponseItem>],
    current_turn_provider_input_tokens: Option<i64>,
    tool_resp_already_recorded: bool,
    recorded_inside_reduce: bool,
) -> Result<SpineHostEffects, SpineError> {
    state
        .prepare_completed_toolcall_for_commit(
            evidence,
            raw_items,
            current_turn_provider_input_tokens,
            tool_resp_already_recorded,
            recorded_inside_reduce,
        )
        .map(|plan| {
            plan.map(|plan| SpineHostEffects::toolcall_commit_loop(plan.into_host_loop()))
                .unwrap_or_else(SpineHostEffects::none)
        })
}
