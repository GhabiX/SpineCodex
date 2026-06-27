use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::TurnContextItem;

use super::super::runtime::SpineError;
use super::super::runtime::SpineSessionState;
use super::HostEffects;

pub(crate) struct MessageRuntime;

impl MessageRuntime {
    pub(crate) fn variable_context_host_effects_if_no_pending_tool_request(
        state: &SpineSessionState,
        raw_items: &[Option<ResponseItem>],
        expected_history: Vec<ResponseItem>,
        reference_context_item: Option<TurnContextItem>,
    ) -> Result<HostEffects, SpineError> {
        state
            .variable_context_host_effects_if_no_pending_tool_request(
                raw_items,
                expected_history,
                reference_context_item,
            )
            .map(HostEffects::from_runtime)
    }
}
