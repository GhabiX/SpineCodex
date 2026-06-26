use codex_protocol::models::ResponseItem;
use std::path::Path;

use super::super::SpineCurrentTrimTarget;
use super::super::SpineError;
use super::super::SpineRuntime;
use super::super::SpineTrimOutcome;
use super::SpineSessionState;
use super::state_types::PreparedSpineReplayRuntime;
use crate::spine::store::SpineStore;

impl SpineSessionState {
    pub(crate) fn trim_tool_response(
        &mut self,
        trim_id: &str,
    ) -> Result<SpineTrimOutcome, SpineError> {
        self.runtime_mut_after_init()?.trim_tool_response(trim_id)
    }

    pub(crate) fn slice_tool_response_head(
        &mut self,
        trim_id: &str,
        head: usize,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<SpineTrimOutcome, SpineError> {
        self.runtime_mut_after_init()?
            .slice_tool_response_head(trim_id, head, raw_items)
    }

    pub(crate) fn slice_tool_response_tail(
        &mut self,
        trim_id: &str,
        tail: usize,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<SpineTrimOutcome, SpineError> {
        self.runtime_mut_after_init()?
            .slice_tool_response_tail(trim_id, tail, raw_items)
    }

    pub(crate) fn slice_tool_response_anchor(
        &mut self,
        trim_id: &str,
        anchor: &str,
        preceding: usize,
        following: usize,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<SpineTrimOutcome, SpineError> {
        self.runtime_mut_after_init()?
            .slice_tool_response_anchor(trim_id, anchor, preceding, following, raw_items)
    }

    pub(crate) fn trim_projection_needs_rollout_raw_items(
        &self,
    ) -> Result<Option<bool>, SpineError> {
        self.with_runtime(|runtime| Ok(runtime.jit_enabled()))
    }

    pub(crate) fn current_trim_targets_for_prompt(
        &self,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<Option<Vec<SpineCurrentTrimTarget>>, SpineError> {
        self.with_runtime(|runtime| runtime.current_trim_targets_for_prompt(raw_items))
    }

    pub(crate) fn materialize_trim_projection_from_raw_items(
        &self,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<Option<Vec<ResponseItem>>, SpineError> {
        self.with_runtime(|runtime| runtime.materialize_variable_context(raw_items))
    }

    pub(crate) fn variable_context_if_no_pending_tool_request(
        &self,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<Option<Vec<ResponseItem>>, SpineError> {
        self.ensure_valid()?;
        let Some(runtime) = self.runtime() else {
            return Ok(None);
        };
        if runtime.has_pending_tool_request() {
            return Ok(None);
        }
        runtime.materialize_variable_context(raw_items).map(Some)
    }

    pub(crate) fn project_trim_projection_from_history(
        &self,
        history_items: &[ResponseItem],
    ) -> Result<Option<Vec<ResponseItem>>, SpineError> {
        self.with_runtime(|runtime| runtime.project_raw_history_with_trim(history_items))
    }

    fn with_runtime<T>(
        &self,
        f: impl FnOnce(&SpineRuntime) -> Result<T, SpineError>,
    ) -> Result<Option<T>, SpineError> {
        self.ensure_valid()?;
        let Some(runtime) = self.runtime() else {
            return Ok(None);
        };
        f(runtime).map(Some)
    }

    pub(crate) fn prepare_trim_replay_from_history(
        rollout_path: &Path,
        raw_len: u64,
        history_items: &[ResponseItem],
    ) -> Result<Option<PreparedSpineReplayRuntime>, SpineError> {
        if !SpineStore::has_for_rollout(rollout_path)? {
            return Ok(None);
        }
        let mut runtime = SpineRuntime::load_or_create_with_jit(rollout_path, raw_len, false)?;
        runtime.set_trim_enabled(true);
        let variable_context = runtime.project_raw_history_with_trim(history_items)?;
        Ok(Some(PreparedSpineReplayRuntime::new(
            raw_len,
            Some(runtime),
            Some(variable_context),
            Vec::new(),
        )))
    }
}
