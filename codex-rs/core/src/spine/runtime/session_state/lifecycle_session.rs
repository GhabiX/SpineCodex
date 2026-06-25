use codex_protocol::models::ResponseItem;
use std::path::Path;

#[cfg(test)]
use super::super::IntoSpineNodeMemory;
use super::super::SpineError;
use super::super::SpineHostEffects;
use super::super::SpineRuntime;
use super::super::support::tool_response_call_id;
use super::SpineInitEvidence;
use super::SpineSessionState;
use super::state_types::PreparedSpineReplayRuntime;
use crate::spine::store::SpineCloneBoundary;
use crate::spine::store::SpineStore;

impl SpineSessionState {
    pub(crate) fn new() -> Self {
        Self::new_with_features(true, true)
    }

    pub(crate) fn new_with_features(jit_enabled: bool, trim_enabled: bool) -> Self {
        Self {
            raw_len: 0,
            runtime: None,
            pending_root_compact_install: None,
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
        self.pending_root_compact_install = None;
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

    pub(crate) fn install_replay(
        &mut self,
        replay: PreparedSpineReplayRuntime,
    ) -> Result<Option<Vec<ResponseItem>>, SpineError> {
        self.set_replayed(replay.raw_len, replay.runtime)?;
        Ok(replay.materialized)
    }

    pub(crate) fn invalidate(&mut self, reason: impl Into<String>) {
        self.pending_root_compact_install = None;
        self.invalid = Some(reason.into());
    }

    pub(crate) fn release_runtime_for_shutdown(&mut self) {
        self.pending_root_compact_install = None;
        self.runtime = None;
    }

    pub(crate) fn release_runtime_for_replay(&mut self) {
        self.pending_root_compact_install = None;
        self.runtime = None;
        self.initial_tree_snapshot_emitted = false;
    }

    pub(crate) fn prepare_jit_replay_from_rollout_items(
        &self,
        rollout_path: &Path,
        raw_len: u64,
        raw_items: &[Option<ResponseItem>],
        rollback_cuts: &[usize],
    ) -> Result<PreparedSpineReplayRuntime, SpineError> {
        self.ensure_valid()?;
        let mut runtime =
            SpineRuntime::load_for_rollout_items(rollout_path, raw_items, rollback_cuts)?;
        if let Some(runtime) = runtime.as_mut() {
            runtime.set_jit_enabled(self.jit_enabled);
            runtime.set_trim_enabled(self.trim_enabled);
        }
        let materialized = runtime
            .as_ref()
            .map(|runtime| runtime.materialize_variable_context(raw_items))
            .transpose()?;
        let live_root_compacts = runtime
            .as_ref()
            .map(|runtime| runtime.live_root_compacts())
            .transpose()?
            .unwrap_or_default();
        Ok(PreparedSpineReplayRuntime::new(
            raw_len,
            runtime,
            materialized,
            live_root_compacts,
        ))
    }

    pub(crate) fn install_cloned_sidecar_for_fork(
        &mut self,
        boundary: &SpineCloneBoundary,
        target_rollout_path: &Path,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<(), SpineError> {
        let raw_live = raw_live_from_items(raw_items);
        SpineStore::clone_for_rollout_with_raw_live(boundary, target_rollout_path, &raw_live)?;
        let raw_ordinal_limit = usize::try_from(boundary.raw_ordinal_limit()).map_err(|_| {
            SpineError::InvalidEvent("clone raw ordinal boundary overflow".to_string())
        })?;
        if raw_ordinal_limit > raw_items.len() {
            return Err(SpineError::InvalidEvent(
                "clone raw ordinal boundary exceeds fork raw length".to_string(),
            ));
        }
        if raw_ordinal_limit == raw_items.len() {
            let runtime = SpineRuntime::load_for_rollout_items_for_writer_with_jit(
                target_rollout_path,
                raw_items,
                &[],
                self.jit_enabled,
            )?;
            return self.set_replayed(raw_item_count(raw_items)?, runtime);
        }

        let prefix_runtime = SpineRuntime::load_for_rollout_items_for_writer_with_jit(
            target_rollout_path,
            &raw_items[..raw_ordinal_limit],
            &[],
            self.jit_enabled,
        )?;
        let mut runtime = prefix_runtime.ok_or_else(|| {
            SpineError::InvalidStore("cloned Spine sidecar is missing after fork clone".to_string())
        })?;
        runtime.set_jit_enabled(self.jit_enabled);
        runtime.set_trim_enabled(self.trim_enabled);

        let mut recorded_tool_outputs = Vec::<(String, u64, usize)>::new();
        for (raw_ordinal, item) in raw_items.iter().enumerate().skip(raw_ordinal_limit) {
            runtime.observe_raw_items(1)?;
            let Some(item) = item.as_ref() else {
                continue;
            };
            let context_index = if runtime.jit_enabled() {
                runtime.variable_context_len(raw_items)?
            } else {
                raw_items
                    .iter()
                    .take(raw_ordinal)
                    .filter(|item| item.is_some())
                    .count()
            };
            let raw_ordinal = u64::try_from(raw_ordinal)
                .map_err(|_| SpineError::InvalidEvent("raw ordinal overflow".to_string()))?;
            runtime.observe_context_item(raw_ordinal, context_index, item)?;
            if let Some(call_id) = tool_response_call_id(item) {
                recorded_tool_outputs.push((call_id.to_string(), raw_ordinal, context_index));
            }
        }
        runtime.observe_recorded_tool_output_group_as_completed_toolcall_with_raw_items(
            &recorded_tool_outputs,
            raw_items,
        )?;
        self.set_replayed(raw_item_count(raw_items)?, Some(runtime))
    }

    pub(crate) fn abort_pending_tool(&mut self, call_id: &str) -> Result<bool, SpineError> {
        self.ensure_valid()?;
        let Some(runtime) = self.runtime_mut() else {
            return Ok(false);
        };
        Ok(runtime.abort_pending(call_id))
    }

    pub(crate) fn abort_any_pending(&mut self) -> Result<Option<String>, SpineError> {
        self.ensure_valid()?;
        let Some(runtime) = self.runtime_mut() else {
            return Ok(None);
        };
        Ok(runtime.abort_any_pending())
    }

    pub(super) fn runtime_mut_after_init(&mut self) -> Result<&mut SpineRuntime, SpineError> {
        self.ensure_valid()?;
        self.runtime_mut().ok_or_else(|| {
            SpineError::InvalidStore("spine runtime missing after initialization".to_string())
        })
    }

    pub(crate) fn ensure_valid(&self) -> Result<(), SpineError> {
        if let Some(reason) = self.invalid.as_ref() {
            return Err(SpineError::Invariant(format!(
                "spine runtime is invalid: {reason}"
            )));
        }
        Ok(())
    }

    pub(crate) fn ensure_observable_context(&self) -> Result<(), SpineError> {
        self.ensure_valid()
    }

    #[cfg(test)]
    pub(crate) fn test_seed_open_control_request(
        &mut self,
        call_id: String,
        summary: String,
    ) -> Result<(), SpineError> {
        self.runtime_mut_after_init()?.stage_open(call_id, summary)
    }

    #[cfg(test)]
    pub(crate) fn test_seed_close_control_request<M: IntoSpineNodeMemory>(
        &mut self,
        call_id: String,
        memory: M,
    ) -> Result<(), SpineError> {
        self.runtime_mut_after_init()?.stage_close(call_id, memory)
    }

    #[cfg(test)]
    pub(crate) fn test_seed_next_control_request<M: IntoSpineNodeMemory>(
        &mut self,
        call_id: String,
        summary: String,
        memory: M,
    ) -> Result<(), SpineError> {
        self.runtime_mut_after_init()?
            .stage_next(call_id, summary, memory)
    }

    pub(crate) fn observe_raw_items(&mut self, count: usize) -> Result<(), SpineError> {
        self.ensure_valid()?;
        let raw_count = u64::try_from(count)
            .map_err(|_| SpineError::InvalidEvent("raw item count overflow".to_string()))?;
        self.raw_len = self
            .raw_len
            .checked_add(raw_count)
            .ok_or_else(|| SpineError::InvalidEvent("raw ordinal overflow".to_string()))?;
        if let Some(runtime) = self.runtime.as_mut() {
            runtime.observe_raw_items(count)?;
        }
        Ok(())
    }

    pub(crate) fn observe_provider_token_usage(&mut self, input_tokens: Option<i64>) {
        if self.ensure_valid().is_err() {
            return;
        }
        let result = {
            let Some(runtime) = self.runtime_mut() else {
                return;
            };
            match input_tokens {
                Some(input_tokens) if input_tokens > 0 => runtime
                    .capture_closed_memory_context_accounting(input_tokens)
                    .map_err(|err| {
                        format!(
                            "failed to capture Spine closed memory context accounting from provider input tokens: {err}"
                        )
                    })
                    .and_then(|_| {
                        runtime
                            .capture_current_open_provider_baseline(input_tokens)
                            .map_err(|err| {
                                format!(
                                    "failed to capture Spine open context baseline from provider input tokens: {err}"
                                )
                            })
                    }),
                Some(_) => runtime
                    .consume_closed_memory_context_accounting_without_provider_usage()
                    .map_err(|err| {
                        format!(
                            "failed to consume Spine closed memory context accounting without positive provider input tokens: {err}"
                        )
                    }),
                None => runtime
                    .consume_closed_memory_context_accounting_without_provider_usage()
                    .map_err(|err| {
                        format!(
                            "failed to consume Spine closed memory context accounting without provider usage: {err}"
                        )
                    }),
            }
        };
        if let Err(reason) = result {
            self.invalidate(reason);
        }
    }

    pub(crate) fn ensure_runtime(&mut self, rollout_path: &Path) -> Result<(), SpineError> {
        self.ensure_valid()?;
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

    pub(in crate::spine) fn on_init(
        &mut self,
        evidence: SpineInitEvidence<'_>,
    ) -> Result<SpineHostEffects, SpineError> {
        self.ensure_runtime(evidence.rollout_path)?;
        self.ensure_valid()?;
        if let Some(runtime) = self.runtime() {
            if runtime.jit_enabled() {
                runtime.checkpoint_initial(evidence.rollout_path, &[])?;
            }
        }
        Ok(SpineHostEffects::none())
    }
}

fn raw_live_from_items(raw_items: &[Option<ResponseItem>]) -> Vec<bool> {
    raw_items.iter().map(Option::is_some).collect()
}

fn raw_item_count(raw_items: &[Option<ResponseItem>]) -> Result<u64, SpineError> {
    u64::try_from(raw_items.len())
        .map_err(|_| SpineError::InvalidEvent("raw item count overflow".to_string()))
}
