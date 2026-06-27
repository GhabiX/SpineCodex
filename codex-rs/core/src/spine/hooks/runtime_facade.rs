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
use super::super::store::SpineStore;
use super::HostEffects;

pub(crate) struct ReplayRuntime {
    inner: runtime::PreparedSpineReplayRuntime,
}

pub(crate) struct ReplayRootCompactBoundary<'a> {
    pub(crate) raw_boundary: u64,
    pub(crate) variable_replacement_history: &'a [ResponseItem],
}

pub(crate) type TrimOutcome = runtime::SpineTrimOutcome;

pub(crate) enum TrimRequest<'a> {
    Snip,
    SliceHead {
        head: usize,
    },
    SliceTail {
        tail: usize,
    },
    SliceAnchor {
        anchor: &'a str,
        preceding: usize,
        following: usize,
    },
}

impl TrimRequest<'_> {
    pub(crate) fn needs_raw_items(&self) -> bool {
        !matches!(self, Self::Snip)
    }
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

    pub(crate) fn into_variable_context(self) -> Option<Vec<ResponseItem>> {
        self.inner.into_variable_context()
    }

    pub(crate) fn validate_rollout_compact_boundaries(
        &self,
        rollout_path: &Path,
        raw_live: &[bool],
        raw_items: &[Option<ResponseItem>],
        base_boundary: ReplayRootCompactBoundary<'_>,
        replacement_history_boundaries: &[ReplayRootCompactBoundary<'_>],
    ) -> Result<(), SpineError> {
        let store = SpineStore::for_rollout(rollout_path)?;
        store.validate_compact_checkpoint_for_boundary(
            rollout_path,
            raw_live,
            raw_items,
            base_boundary.raw_boundary,
            base_boundary.variable_replacement_history,
        )?;
        self.validate_live_root_compacts_have_rollout_boundary_proofs(
            replacement_history_boundaries,
            &store,
            rollout_path,
            raw_live,
            raw_items,
        )
    }

    pub(crate) fn validate_no_rollout_compact_boundaries(&self) -> Result<(), SpineError> {
        if let Some(compact) = self.inner.live_root_compacts().first() {
            return Err(SpineError::InvalidStore(format!(
                "spine_jit root compact sidecar is missing rollout compact boundary at raw boundary {} token_seq {}",
                compact.raw_boundary, compact.token_seq
            )));
        }
        Ok(())
    }

    fn validate_live_root_compacts_have_rollout_boundary_proofs(
        &self,
        replacement_history_boundaries: &[ReplayRootCompactBoundary<'_>],
        store: &SpineStore,
        rollout_path: &Path,
        raw_live: &[bool],
        raw_items: &[Option<ResponseItem>],
    ) -> Result<(), SpineError> {
        for compact in self.inner.live_root_compacts() {
            if Self::prove_live_root_compact_with_rollout_boundary(
                *compact,
                replacement_history_boundaries,
                store,
                rollout_path,
                raw_live,
                raw_items,
            )?
            .is_none()
            {
                return Err(SpineError::InvalidStore(format!(
                    "spine_jit root compact sidecar is missing rollout compact boundary at raw boundary {} token_seq {}",
                    compact.raw_boundary, compact.token_seq
                )));
            }
        }
        Ok(())
    }

    fn prove_live_root_compact_with_rollout_boundary(
        compact: runtime::LiveRootCompact,
        replacement_history_boundaries: &[ReplayRootCompactBoundary<'_>],
        store: &SpineStore,
        rollout_path: &Path,
        raw_live: &[bool],
        raw_items: &[Option<ResponseItem>],
    ) -> Result<Option<()>, SpineError> {
        let mut saw_same_boundary = false;
        for boundary in replacement_history_boundaries
            .iter()
            .filter(|boundary| boundary.raw_boundary == compact.raw_boundary)
        {
            saw_same_boundary = true;
            let checkpoint_token_seq = store.validate_compact_checkpoint_for_boundary(
                rollout_path,
                raw_live,
                raw_items,
                boundary.raw_boundary,
                boundary.variable_replacement_history,
            )?;
            if checkpoint_token_seq.checked_sub(1) == Some(compact.token_seq) {
                return Ok(Some(()));
            }
        }
        if saw_same_boundary {
            return Err(SpineError::InvalidStore(format!(
                "spine compact checkpoint token_seq does not match live RootCompact at raw boundary {} token_seq {}",
                compact.raw_boundary, compact.token_seq
            )));
        }
        Ok(None)
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
    ) -> Result<TrimOutcome, SpineError> {
        state.trim_tool_response(trim_id)
    }

    pub(crate) fn slice_tool_response_head(
        state: &mut SpineSessionState,
        trim_id: &str,
        head: usize,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<TrimOutcome, SpineError> {
        state.slice_tool_response_head(trim_id, head, raw_items)
    }

    pub(crate) fn slice_tool_response_tail(
        state: &mut SpineSessionState,
        trim_id: &str,
        tail: usize,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<TrimOutcome, SpineError> {
        state.slice_tool_response_tail(trim_id, tail, raw_items)
    }

    pub(crate) fn slice_tool_response_anchor(
        state: &mut SpineSessionState,
        trim_id: &str,
        anchor: &str,
        preceding: usize,
        following: usize,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<TrimOutcome, SpineError> {
        state.slice_tool_response_anchor(trim_id, anchor, preceding, following, raw_items)
    }

    pub(crate) fn apply_tool_response_request(
        state: &mut SpineSessionState,
        trim_id: &str,
        request: TrimRequest<'_>,
        raw_items: Option<&[Option<ResponseItem>]>,
    ) -> Result<TrimOutcome, SpineError> {
        match request {
            TrimRequest::Snip => Self::trim_tool_response(state, trim_id),
            TrimRequest::SliceHead { head } => {
                let raw_items = raw_items.ok_or_else(|| {
                    SpineError::InvalidEvent(
                        "spine trim slice_head requires raw rollout items".to_string(),
                    )
                })?;
                Self::slice_tool_response_head(state, trim_id, head, raw_items)
            }
            TrimRequest::SliceTail { tail } => {
                let raw_items = raw_items.ok_or_else(|| {
                    SpineError::InvalidEvent(
                        "spine trim slice_tail requires raw rollout items".to_string(),
                    )
                })?;
                Self::slice_tool_response_tail(state, trim_id, tail, raw_items)
            }
            TrimRequest::SliceAnchor {
                anchor,
                preceding,
                following,
            } => {
                let raw_items = raw_items.ok_or_else(|| {
                    SpineError::InvalidEvent(
                        "spine trim slice_anchor requires raw rollout items".to_string(),
                    )
                })?;
                Self::slice_tool_response_anchor(
                    state, trim_id, anchor, preceding, following, raw_items,
                )
            }
        }
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
