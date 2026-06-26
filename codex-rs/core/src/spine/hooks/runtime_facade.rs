use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::TurnContextItem;
#[cfg(test)]
use codex_protocol::spine_tree::SpineTreeUpdateEvent;
use std::path::Path;

use super::super::SpineCloneBoundary;
use super::super::runtime;
#[cfg(test)]
use super::super::runtime::IntoSpineNodeMemory;
use super::super::runtime::SpineError;
use super::super::runtime::SpineSessionState;
use super::HostEffects;

pub(crate) struct ReplayRuntime {
    inner: runtime::PreparedSpineReplayRuntime,
}

pub(crate) struct LifecycleRuntime;

pub(crate) struct TrimRuntime;

pub(crate) struct MessageRuntime;

#[cfg(test)]
pub(crate) struct TestRuntime;

impl ReplayRuntime {
    pub(crate) fn has_runtime(&self) -> bool {
        self.inner.has_runtime()
    }

    pub(crate) fn live_root_compacts(&self) -> &[runtime::LiveRootCompact] {
        self.inner.live_root_compacts()
    }

    pub(crate) fn into_materialized(self) -> Option<Vec<ResponseItem>> {
        self.inner.into_materialized()
    }

    pub(crate) fn prepare_jit_replay_from_rollout_items(
        state: &SpineSessionState,
        rollout_path: &Path,
        raw_len: u64,
        raw_items: &[Option<ResponseItem>],
        rollback_cuts: &[usize],
    ) -> Result<Self, SpineError> {
        state
            .prepare_jit_replay_from_rollout_items(rollout_path, raw_len, raw_items, rollback_cuts)
            .map(|inner| Self { inner })
    }

    pub(crate) fn prepare_trim_replay_from_history(
        rollout_path: &Path,
        raw_len: u64,
        history_items: &[ResponseItem],
    ) -> Result<Option<Self>, SpineError> {
        SpineSessionState::prepare_trim_replay_from_history(rollout_path, raw_len, history_items)
            .map(|replay| replay.map(|inner| Self { inner }))
    }

    pub(crate) fn install(
        self,
        state: &mut SpineSessionState,
    ) -> Result<Option<Vec<ResponseItem>>, SpineError> {
        state.install_replay(self.inner)
    }
}

impl LifecycleRuntime {
    pub(crate) fn is_ready(state: &SpineSessionState) -> bool {
        state.is_ready()
    }

    pub(crate) fn ensure_runtime(
        state: &mut SpineSessionState,
        rollout_path: &Path,
    ) -> Result<(), SpineError> {
        state.ensure_runtime(rollout_path)
    }

    pub(crate) fn invalidate(state: &mut SpineSessionState, reason: String) {
        state.invalidate(reason);
    }

    pub(crate) fn release_runtime_for_shutdown(state: &mut SpineSessionState) {
        state.release_runtime_for_shutdown();
    }

    pub(crate) fn release_runtime_for_replay(state: &mut SpineSessionState) {
        state.release_runtime_for_replay();
    }

    pub(crate) fn install_cloned_sidecar_for_fork(
        state: &mut SpineSessionState,
        boundary: &SpineCloneBoundary,
        target_rollout_path: &Path,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<(), SpineError> {
        state.install_cloned_sidecar_for_fork(boundary, target_rollout_path, raw_items)
    }

    pub(crate) fn observe_raw_items(
        state: &mut SpineSessionState,
        count: usize,
    ) -> Result<(), SpineError> {
        state.observe_raw_items(count)
    }

    pub(crate) fn ensure_observable_context(state: &SpineSessionState) -> Result<(), SpineError> {
        state.ensure_observable_context()
    }

    pub(crate) fn observe_toolcall_context_item_facts<'a>(
        state: &mut SpineSessionState,
        items: impl IntoIterator<Item = (u64, usize, &'a ResponseItem)>,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<(), SpineError> {
        state.observe_toolcall_context_item_facts(items, raw_items)
    }

    pub(crate) fn abort_pending_tool(
        state: &mut SpineSessionState,
        call_id: &str,
    ) -> Result<bool, SpineError> {
        state.abort_pending_tool(call_id)
    }

    pub(crate) fn abort_any_pending(
        state: &mut SpineSessionState,
    ) -> Result<Option<String>, SpineError> {
        state.abort_any_pending()
    }

    pub(crate) fn is_control_output_call_id(
        state: &SpineSessionState,
        call_id: &str,
    ) -> Result<bool, SpineError> {
        state.is_control_output_call_id(call_id)
    }
}

impl TrimRuntime {
    pub(crate) fn projection_needs_rollout_raw_items(
        state: &SpineSessionState,
    ) -> Result<Option<bool>, SpineError> {
        state.trim_projection_needs_rollout_raw_items()
    }

    pub(crate) fn materialize_projection_from_raw_items(
        state: &SpineSessionState,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<Option<Vec<ResponseItem>>, SpineError> {
        state.materialize_trim_projection_from_raw_items(raw_items)
    }

    pub(crate) fn project_from_history(
        state: &SpineSessionState,
        history_items: &[ResponseItem],
    ) -> Result<Option<Vec<ResponseItem>>, SpineError> {
        state.project_trim_projection_from_history(history_items)
    }

    pub(crate) fn trim_tool_response(
        state: &mut SpineSessionState,
        trim_id: &str,
    ) -> Result<runtime::SpineTrimOutcome, SpineError> {
        state.trim_tool_response(trim_id)
    }

    pub(crate) fn slice_tool_response_head(
        state: &mut SpineSessionState,
        trim_id: &str,
        head: usize,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<runtime::SpineTrimOutcome, SpineError> {
        state.slice_tool_response_head(trim_id, head, raw_items)
    }

    pub(crate) fn slice_tool_response_tail(
        state: &mut SpineSessionState,
        trim_id: &str,
        tail: usize,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<runtime::SpineTrimOutcome, SpineError> {
        state.slice_tool_response_tail(trim_id, tail, raw_items)
    }

    pub(crate) fn slice_tool_response_anchor(
        state: &mut SpineSessionState,
        trim_id: &str,
        anchor: &str,
        preceding: usize,
        following: usize,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<runtime::SpineTrimOutcome, SpineError> {
        state.slice_tool_response_anchor(trim_id, anchor, preceding, following, raw_items)
    }
}

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

#[cfg(test)]
impl TestRuntime {
    pub(crate) fn seed_open_control_request(
        state: &mut SpineSessionState,
        call_id: String,
        summary: String,
    ) -> Result<(), SpineError> {
        state.test_seed_open_control_request(call_id, summary)
    }

    pub(crate) fn seed_close_control_request<M: IntoSpineNodeMemory>(
        state: &mut SpineSessionState,
        call_id: String,
        memory: M,
    ) -> Result<(), SpineError> {
        state.test_seed_close_control_request(call_id, memory)
    }

    pub(crate) fn seed_next_control_request<M: IntoSpineNodeMemory>(
        state: &mut SpineSessionState,
        call_id: String,
        summary: String,
        memory: M,
    ) -> Result<(), SpineError> {
        state.test_seed_next_control_request(call_id, summary, memory)
    }

    pub(crate) fn is_ready(state: &SpineSessionState) -> Result<bool, SpineError> {
        state.ensure_valid()?;
        Ok(LifecycleRuntime::is_ready(state))
    }

    pub(crate) fn prepare_native_root_compact_apply_with_checkpoint(
        state: &mut SpineSessionState,
        rollout_path: &Path,
        body: String,
        raw_items: &[Option<ResponseItem>],
        close_provider_input_tokens: Option<i64>,
    ) -> Result<runtime::SpineRootCompactHostInstall, SpineError> {
        state.prepare_native_root_compact_apply_with_checkpoint(
            rollout_path,
            body,
            raw_items,
            close_provider_input_tokens,
        )
    }

    pub(crate) fn apply_root_compact_after_history_publish(
        state: &mut SpineSessionState,
        prepared: runtime::SpineRootCompactHostInstall,
        published_variable_context_len: usize,
    ) -> Result<SpineTreeUpdateEvent, SpineError> {
        state.apply_root_compact_after_history_publish(prepared, published_variable_context_len)
    }
}
