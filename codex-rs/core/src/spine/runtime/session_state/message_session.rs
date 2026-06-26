use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::TurnContextItem;

use super::super::SpineError;
use super::super::SpineHistoryUpdate;
use super::super::SpineHostEffects;
use super::super::support::is_non_toolcall_msg;
use super::super::support::is_real_user_message;
use super::SpineSessionState;
use super::state_types::SpineMessageEvidence;

impl SpineSessionState {
    pub(in crate::spine) fn observe_non_toolcall_msg_with_host_effects(
        &mut self,
        evidence: SpineMessageEvidence<'_>,
    ) -> Result<SpineHostEffects, SpineError> {
        self.ensure_valid()?;
        let Some(runtime) = self.runtime_mut() else {
            return Ok(SpineHostEffects::none());
        };
        if !is_non_toolcall_msg(evidence.item) {
            return Err(SpineError::InvalidEvent(
                "on_non_toolcall_msg received toolcall item".to_string(),
            ));
        }
        let observed_user_message = is_real_user_message(evidence.item);
        if runtime.jit_enabled() && observed_user_message {
            runtime.checkpoint_before_user_msg(
                evidence.rollout_path,
                evidence.raw_ordinal,
                evidence.raw_items,
            )?;
        }
        runtime.on_non_toolcall_msg(evidence.raw_ordinal, evidence.context_index, evidence.item)?;
        if !observed_user_message {
            return Ok(SpineHostEffects::none());
        }
        Ok(SpineHostEffects::publish_variable_context_after_batch())
    }

    pub(crate) fn variable_context_host_effects_if_no_pending_tool_request(
        &self,
        raw_items: &[Option<ResponseItem>],
        expected_history: Vec<ResponseItem>,
        reference_context_item: Option<TurnContextItem>,
    ) -> Result<SpineHostEffects, SpineError> {
        let Some(replacement) = self.materialize_history_if_no_pending_tool_request(raw_items)?
        else {
            return Ok(SpineHostEffects::none());
        };
        if replacement == expected_history {
            return Ok(SpineHostEffects::none());
        }
        Ok(SpineHostEffects::replace_history(SpineHistoryUpdate {
            call_id: "non-toolcall-msg".to_string(),
            operation: "publish Spine h(PS) after non-toolcall message",
            suffix_start: 0,
            expected_history,
            replacement,
            reference_context_item,
        }))
    }
}
