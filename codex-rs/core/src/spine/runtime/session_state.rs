use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::TurnContextItem;
use codex_protocol::spine_tree::SpineTreeUpdateEvent;
use std::path::Path;

use super::CompletedToolCall;
use super::SpineError;
use super::SpineHistoryUpdate;
use super::SpineHostEffects;
use super::SpinePreparedRootCompactInstall;
#[cfg(test)]
use super::SpineRootCompactResult;
use super::SpineRuntime;
use super::SpineTreeUpdateDelivery;
use super::prepared::SpineCommitPublication;
use super::types::SpinePreparedCloseMemory;

pub(crate) struct PreparedSpineToolcallCommit {
    publication: SpineCommitPublication<SpineHistoryUpdate>,
}

pub(crate) struct SpinePostApplyEffectPolicy {
    delivery: SpineTreeUpdateDelivery,
}

impl PreparedSpineToolcallCommit {
    fn new(publication: SpineCommitPublication<SpineHistoryUpdate>) -> Self {
        Self { publication }
    }

    pub(crate) fn take_pre_apply_host_effects(&mut self) -> SpineHostEffects {
        SpineHostEffects::from_optional_history_update(self.publication.take_history_update())
    }

    pub(crate) fn post_apply_effect_policy(&self) -> SpinePostApplyEffectPolicy {
        let delivery = if self.publication.defer_tree_update_until_raw_output() {
            SpineTreeUpdateDelivery::AfterRawOutputDurable
        } else {
            SpineTreeUpdateDelivery::Immediate
        };
        SpinePostApplyEffectPolicy { delivery }
    }
}

impl SpinePostApplyEffectPolicy {
    pub(crate) fn host_effects(self, snapshot: Option<SpineTreeUpdateEvent>) -> SpineHostEffects {
        SpineHostEffects::from_optional_tree_update(snapshot, self.delivery)
    }
}

pub(crate) struct PreparedSpineRootCompactCommit {
    install: SpinePreparedRootCompactInstall,
}

impl PreparedSpineRootCompactCommit {
    pub(crate) fn from_install(install: SpinePreparedRootCompactInstall) -> Self {
        Self { install }
    }

    pub(crate) fn materialized(&self) -> &[ResponseItem] {
        &self.install.result().materialized
    }

    #[cfg(test)]
    pub(crate) fn result(&self) -> SpineRootCompactResult {
        self.install.result().clone()
    }
}

#[derive(Debug)]
pub(crate) struct SpineSessionState {
    raw_len: u64,
    runtime: Option<SpineRuntime>,
    jit_enabled: bool,
    trim_enabled: bool,
    initial_tree_snapshot_emitted: bool,
    invalid: Option<String>,
}

impl SpineSessionState {
    pub(crate) fn new() -> Self {
        Self::new_with_features(true, true)
    }

    pub(crate) fn new_with_features(jit_enabled: bool, trim_enabled: bool) -> Self {
        Self {
            raw_len: 0,
            runtime: None,
            jit_enabled,
            trim_enabled,
            initial_tree_snapshot_emitted: false,
            invalid: None,
        }
    }

    pub(crate) fn runtime(&self) -> Option<&SpineRuntime> {
        if self.invalid.is_some() {
            return None;
        }
        self.runtime.as_ref()
    }

    pub(crate) fn runtime_mut(&mut self) -> Option<&mut SpineRuntime> {
        if self.invalid.is_some() {
            return None;
        }
        self.runtime.as_mut()
    }

    pub(crate) fn is_ready(&self) -> bool {
        self.invalid.is_none() && self.runtime.is_some()
    }

    pub(crate) fn raw_len(&self) -> u64 {
        self.raw_len
    }

    pub(crate) fn set_replayed(
        &mut self,
        raw_len: u64,
        mut runtime: Option<SpineRuntime>,
    ) -> Result<(), SpineError> {
        drop(self.runtime.take());
        if let Some(runtime) = runtime.as_mut() {
            runtime.set_jit_enabled(self.jit_enabled);
            runtime.set_trim_enabled(self.trim_enabled);
            runtime.acquire_writer_lock()?;
        }
        self.raw_len = raw_len;
        self.runtime = runtime;
        self.initial_tree_snapshot_emitted = false;
        self.invalid = None;
        Ok(())
    }

    pub(crate) fn invalidate(&mut self, reason: impl Into<String>) {
        self.invalid = Some(reason.into());
    }

    pub(crate) fn release_runtime_for_shutdown(&mut self) {
        self.runtime = None;
    }

    pub(crate) fn release_runtime_for_replay(&mut self) {
        self.runtime = None;
        self.initial_tree_snapshot_emitted = false;
    }

    pub(crate) fn abort_pending_tool(&mut self, call_id: &str) -> bool {
        let Some(runtime) = self.runtime_mut() else {
            return false;
        };
        runtime.abort_pending(call_id)
    }

    pub(crate) fn abort_any_pending(&mut self) -> Option<String> {
        let runtime = self.runtime_mut()?;
        runtime.abort_any_pending()
    }

    fn invalid_error(&self) -> Option<SpineError> {
        self.invalid
            .as_ref()
            .map(|reason| SpineError::Invariant(format!("spine runtime is invalid: {reason}")))
    }

    pub(crate) fn ensure_valid(&self) -> Result<(), SpineError> {
        if let Some(err) = self.invalid_error() {
            return Err(err);
        }
        Ok(())
    }

    pub(crate) fn observe_raw_items(&mut self, count: usize) -> Result<(), SpineError> {
        if let Some(err) = self.invalid_error() {
            return Err(err);
        }
        let count = u64::try_from(count)
            .map_err(|_| SpineError::InvalidEvent("raw item count overflow".to_string()))?;
        self.raw_len = self
            .raw_len
            .checked_add(count)
            .ok_or_else(|| SpineError::InvalidEvent("raw ordinal overflow".to_string()))?;
        if let Some(runtime) = self.runtime.as_mut() {
            let count = usize::try_from(count)
                .map_err(|_| SpineError::InvalidEvent("raw item count overflow".to_string()))?;
            runtime.observe_raw_items(count)?;
        }
        Ok(())
    }

    pub(crate) fn ensure_runtime(&mut self, rollout_path: &Path) -> Result<(), SpineError> {
        if let Some(err) = self.invalid_error() {
            return Err(err);
        }
        if self.runtime.is_none() {
            let mut runtime = SpineRuntime::load_or_create_with_jit(
                rollout_path,
                self.raw_len,
                self.jit_enabled,
            )?;
            runtime.set_jit_enabled(self.jit_enabled);
            runtime.set_trim_enabled(self.trim_enabled);
            self.runtime = Some(runtime);
        }
        Ok(())
    }

    pub(crate) fn take_initial_tree_snapshot(
        &mut self,
    ) -> Result<Option<SpineTreeUpdateEvent>, SpineError> {
        if let Some(err) = self.invalid_error() {
            return Err(err);
        }
        if self.initial_tree_snapshot_emitted {
            return Ok(None);
        }
        let Some(runtime) = self.runtime.as_ref() else {
            return Ok(None);
        };
        if !runtime.jit_enabled() {
            return Ok(None);
        }
        let snapshot = runtime.build_tree_snapshot()?;
        self.initial_tree_snapshot_emitted = true;
        Ok(Some(snapshot))
    }

    pub(crate) fn apply_root_compact_after_history_publish(
        &mut self,
        commit: PreparedSpineRootCompactCommit,
        published_history_len: usize,
    ) -> Result<SpineTreeUpdateEvent, SpineError> {
        self.ensure_valid()?;
        let Some(runtime) = self.runtime_mut() else {
            return Err(SpineError::InvalidStore(
                "spine runtime missing before root compact PS install".to_string(),
            ));
        };
        runtime.install_prepared_root_compact_install(commit.install);
        let current_open_index = runtime.current_open_index()?;
        if current_open_index != published_history_len {
            return Err(SpineError::InvalidStore(format!(
                "spine root compact open index {current_open_index} does not match materialized history length {published_history_len}"
            )));
        }
        runtime.build_tree_snapshot()
    }

    pub(crate) fn prepare_completed_toolcall_commit(
        &mut self,
        call_id: &str,
        memory: Option<SpinePreparedCloseMemory>,
        pre_compact_provider_input_tokens: Option<i64>,
        current_turn_provider_input_tokens: Option<i64>,
        completed_toolcall: CompletedToolCall,
        tool_resp_item: &ResponseItem,
        tool_resp_already_recorded: bool,
        raw_items: &[Option<ResponseItem>],
        history_items: &[ResponseItem],
        reference_context_item: Option<TurnContextItem>,
    ) -> Result<Option<PreparedSpineToolcallCommit>, SpineError> {
        self.ensure_valid()?;
        let Some(runtime) = self.runtime_mut() else {
            return Ok(None);
        };
        runtime.validate_close_expected_history_for_commit(
            call_id,
            memory
                .as_ref()
                .map(SpinePreparedCloseMemory::expected_history),
            history_items,
        )?;
        let memory_assembly = memory.map(SpinePreparedCloseMemory::into_assembly);
        let commit_application = runtime
            .prepare_or_observe_completed_toolcall_with_pending_baselines(
                call_id,
                memory_assembly,
                pre_compact_provider_input_tokens,
                current_turn_provider_input_tokens,
                completed_toolcall,
                raw_items,
            )?;
        runtime
            .prepare_commit_publication(
                call_id,
                commit_application,
                tool_resp_item,
                tool_resp_already_recorded,
                raw_items,
                history_items,
                |call_id, operation, suffix_start, expected_history, replacement| {
                    SpineHistoryUpdate {
                        call_id: call_id.to_string(),
                        operation,
                        suffix_start,
                        expected_history,
                        replacement,
                        reference_context_item,
                    }
                },
            )
            .map(PreparedSpineToolcallCommit::new)
            .map(Some)
    }

    pub(crate) fn persist_toolcall_commit_side_effects(
        &mut self,
        prepared: &PreparedSpineToolcallCommit,
    ) -> Result<(), SpineError> {
        self.ensure_valid()?;
        let Some(runtime) = self.runtime_mut() else {
            return Err(SpineError::InvalidStore(
                "spine runtime missing before commit publication side effects".to_string(),
            ));
        };
        runtime.persist_commit_publication_side_effects(&prepared.publication)
    }

    pub(crate) fn apply_toolcall_commit(
        &mut self,
        prepared: PreparedSpineToolcallCommit,
    ) -> Result<bool, SpineError> {
        self.ensure_valid()?;
        let Some(runtime) = self.runtime_mut() else {
            return Err(SpineError::InvalidStore(
                "spine runtime missing before commit publication install".to_string(),
            ));
        };
        Ok(runtime.install_commit_publication(prepared.publication))
    }
}
