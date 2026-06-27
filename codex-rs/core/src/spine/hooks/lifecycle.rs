use codex_protocol::models::ResponseItem;
use std::path::Path;

use super::super::SpineCloneBoundary;
use super::super::runtime::SpineError;
use super::super::runtime::SpineSessionState;

pub(crate) type ForkCloneBoundary = SpineCloneBoundary;

pub(crate) struct LifecycleRuntime;

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
