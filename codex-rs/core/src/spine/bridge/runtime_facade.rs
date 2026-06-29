use codex_protocol::models::ResponseItem;
#[cfg(test)]
use codex_protocol::spine_tree::SpineTreeUpdateEvent;
use std::path::Path;

#[cfg(test)]
use super::super::IntoSpineNodeMemory;
#[cfg(test)]
use super::super::SpineRootCompactHostInstall;
use super::super::runtime::SpineError;
use super::super::runtime::SpineSessionState;

#[cfg(test)]
pub(crate) struct TestRuntime;

#[cfg(test)]
impl TestRuntime {
    pub(crate) fn seed_open_control_request(
        state: &mut SpineSessionState,
        call_id: String,
        summary: String,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<(), SpineError> {
        state.test_seed_open_control_request(call_id, summary, raw_items)
    }

    pub(crate) fn seed_close_control_request<M: IntoSpineNodeMemory>(
        state: &mut SpineSessionState,
        call_id: String,
        memory: M,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<(), SpineError> {
        state.test_seed_close_control_request(call_id, memory, raw_items)
    }

    pub(crate) fn seed_next_control_request<M: IntoSpineNodeMemory>(
        state: &mut SpineSessionState,
        call_id: String,
        summary: String,
        memory: M,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<(), SpineError> {
        state.test_seed_next_control_request(call_id, summary, memory, raw_items)
    }

    pub(crate) fn is_ready(state: &SpineSessionState) -> Result<bool, SpineError> {
        state.ensure_valid()?;
        Ok(state.is_ready())
    }

    pub(crate) fn prepare_native_root_compact_apply_with_checkpoint(
        state: &mut SpineSessionState,
        rollout_path: &Path,
        body: String,
        raw_items: &[Option<ResponseItem>],
        close_provider_input_tokens: Option<i64>,
    ) -> Result<SpineRootCompactHostInstall, SpineError> {
        state.prepare_native_root_compact_apply_with_checkpoint(
            rollout_path,
            body,
            raw_items,
            close_provider_input_tokens,
        )
    }

    pub(crate) fn apply_root_compact_after_history_publish(
        state: &mut SpineSessionState,
        prepared: SpineRootCompactHostInstall,
        published_variable_context_len: usize,
    ) -> Result<SpineTreeUpdateEvent, SpineError> {
        state.apply_root_compact_after_history_publish(prepared, published_variable_context_len)
    }
}
