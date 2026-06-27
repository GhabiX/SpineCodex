use codex_protocol::models::ResponseItem;
#[cfg(test)]
use codex_protocol::spine_tree::SpineTreeUpdateEvent;
use std::path::Path;

use super::super::SpineCloneBoundary;
use super::super::runtime;
#[cfg(test)]
pub(crate) use super::super::runtime::IntoSpineNodeMemory as TestNodeMemoryInput;
use super::super::runtime::SpineError;
use super::super::runtime::SpineSessionState;
use super::super::store::SpineStore;

pub(crate) struct ReplayRuntime {
    inner: runtime::PreparedSpineReplayRuntime,
}

pub(crate) struct ReplayRootCompactBoundary<'a> {
    pub(crate) raw_boundary: u64,
    pub(crate) variable_replacement_history: &'a [ResponseItem],
}

pub(crate) type ForkCloneBoundary = SpineCloneBoundary;

pub(crate) struct LifecycleRuntime;

#[cfg(test)]
pub(crate) struct TestRuntime;

#[cfg(test)]
pub(crate) type TestRootCompactHostInstall = runtime::SpineRootCompactHostInstall;

#[cfg(test)]
pub(crate) type TestRootCompactResult = runtime::SpineRootCompactResult;

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

    pub(crate) fn seed_close_control_request<M: TestNodeMemoryInput>(
        state: &mut SpineSessionState,
        call_id: String,
        memory: M,
    ) -> Result<(), SpineError> {
        state.test_seed_close_control_request(call_id, memory)
    }

    pub(crate) fn seed_next_control_request<M: TestNodeMemoryInput>(
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
    ) -> Result<TestRootCompactHostInstall, SpineError> {
        state.prepare_native_root_compact_apply_with_checkpoint(
            rollout_path,
            body,
            raw_items,
            close_provider_input_tokens,
        )
    }

    pub(crate) fn apply_root_compact_after_history_publish(
        state: &mut SpineSessionState,
        prepared: TestRootCompactHostInstall,
        published_variable_context_len: usize,
    ) -> Result<SpineTreeUpdateEvent, SpineError> {
        state.apply_root_compact_after_history_publish(prepared, published_variable_context_len)
    }
}
