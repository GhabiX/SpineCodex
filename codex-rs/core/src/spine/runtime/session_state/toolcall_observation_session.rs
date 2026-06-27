use codex_protocol::models::ResponseItem;

use super::super::SpineError;
use super::super::SpineHostEffects;
use super::SpineSessionState;
use super::completed_toolcall_evidence::SpineCompletedToolCallOutputEvidence;
use super::completed_toolcall_evidence::SpineCompletedToolCallRequestIds;
use super::completed_toolcall_evidence::SpineToolcallCommitEvidence;
use super::completed_toolcall_evidence::SpineToolcallHookEvidence;
use super::completed_toolcall_evidence::assign_response_item_raw_ordinals;
use super::completed_toolcall_evidence::completed_toolcall_evidence_from_segments;
use super::completed_toolcall_evidence::completed_toolcall_request_segment;
use super::completed_toolcall_evidence::completed_toolcall_request_segments;
use super::completed_toolcall_evidence::completed_toolcall_response_segment;
use super::completed_toolcall_evidence::completed_toolcall_response_segments;
use super::state_types::SpineGroupedToolcallOutputRecordingPlan;
use super::state_types::SpineSingleToolcallOutputRecordingPlan;
use super::state_types::SpineToolcallOutputRecordingPlan;
use super::state_types::SpineToolcallOutputRecordingRequest;
use super::toolcall_host_commit::SpineToolcallCommitPreparation;

impl SpineSessionState {
    pub(crate) fn prepare_single_toolcall_output_recording(
        &self,
        call_id: &str,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<Option<SpineSingleToolcallOutputRecordingPlan>, SpineError> {
        self.ensure_valid()?;
        let Some(runtime) = self.runtime() else {
            return Ok(None);
        };
        Ok(Some(SpineSingleToolcallOutputRecordingPlan {
            raw_len: self.raw_len,
            prerecord_output_before_reduce: runtime
                .has_close_like_control_request(call_id, raw_items)?,
        }))
    }

    pub(crate) fn prepare_grouped_toolcall_output_recording(
        &self,
        output_items: &[ResponseItem],
    ) -> Result<SpineGroupedToolcallOutputRecordingPlan, SpineError> {
        self.ensure_valid()?;
        Ok(SpineGroupedToolcallOutputRecordingPlan {
            raw_ordinals: assign_response_item_raw_ordinals(self.raw_len, output_items)?,
        })
    }

    pub(crate) fn prepare_toolcall_output_recording(
        &self,
        request: SpineToolcallOutputRecordingRequest<'_>,
    ) -> Result<SpineToolcallOutputRecordingPlan, SpineError> {
        match request {
            SpineToolcallOutputRecordingRequest::Single { call_id, raw_items } => self
                .prepare_single_toolcall_output_recording(call_id, raw_items)
                .map(SpineToolcallOutputRecordingPlan::Single),
            SpineToolcallOutputRecordingRequest::Grouped { output_items } => self
                .prepare_grouped_toolcall_output_recording(output_items)
                .map(SpineToolcallOutputRecordingPlan::Grouped),
        }
    }

    pub(crate) fn is_control_output_call_id(&self, call_id: &str) -> Result<bool, SpineError> {
        self.ensure_valid()?;
        let Some(runtime) = self.runtime() else {
            return Ok(false);
        };
        Ok(runtime.is_control_output_call_id(call_id))
    }

    pub(in crate::spine) fn prepare_completed_toolcall_for_commit(
        &mut self,
        evidence: SpineToolcallHookEvidence<'_>,
    ) -> Result<SpineHostEffects, SpineError> {
        self.ensure_valid()?;
        let Some(commit_evidence) = self.completed_toolcall_commit_evidence_from_output(
            evidence.completed_output,
            evidence.output_raw_ordinals,
            evidence.output_context_start,
            evidence.raw_items,
        )?
        else {
            return Ok(SpineHostEffects::none());
        };
        let Some(runtime) = self.runtime_mut() else {
            return Ok(SpineHostEffects::none());
        };
        let call_id = commit_evidence.call_id.as_str();
        let force_ordinary = commit_evidence.force_ordinary();
        if !force_ordinary {
            runtime.ensure_pending_from_toolcall_request(call_id, evidence.raw_items)?;
        }
        let has_close_like_control = !force_ordinary
            && runtime.has_close_like_control_request(call_id, evidence.raw_items)?;
        let preparation = SpineToolcallCommitPreparation::new(has_close_like_control);
        let plan = preparation.host_plan(
            evidence.current_turn_provider_input_tokens,
            evidence.tool_resp_already_recorded,
            evidence.recorded_inside_reduce,
        );
        Ok(SpineHostEffects::toolcall_host_commit(
            plan.into_host_commit(commit_evidence),
        ))
    }

    pub(in crate::spine) fn single_completed_toolcall_evidence(
        &self,
        call_id: &str,
        response_anchor: (u64, usize),
        raw_items: &[Option<ResponseItem>],
    ) -> Result<Option<SpineToolcallCommitEvidence>, SpineError> {
        self.ensure_valid()?;
        let Some(runtime) = self.runtime() else {
            return Ok(None);
        };
        let request_anchor = runtime
            .pending_tool_request_anchor(call_id)
            .or_else(|_| runtime.tool_request_anchor_from_raw_items(call_id, raw_items))?;
        let completed_toolcall = completed_toolcall_evidence_from_segments(
            call_id,
            &[call_id.to_string()],
            vec![completed_toolcall_request_segment(
                request_anchor.raw_ordinal,
                request_anchor.context_index,
            )],
            vec![completed_toolcall_response_segment(
                response_anchor.0,
                response_anchor.1,
            )],
            "completed toolcall must contain a request",
            "completed toolcall must contain a response",
        )?;
        Ok(Some(SpineToolcallCommitEvidence::new(
            call_id,
            completed_toolcall,
        )))
    }

    pub(in crate::spine) fn grouped_completed_toolcall_evidence(
        &self,
        commit_call_id: &str,
        tool_call_ids: &[String],
        response_raw_ordinals: &[Option<u64>],
        response_context_start: usize,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<Option<SpineToolcallCommitEvidence>, SpineError> {
        self.ensure_valid()?;
        let Some(runtime) = self.runtime() else {
            return Ok(None);
        };
        let request_anchors = tool_call_ids
            .iter()
            .map(|call_id| {
                runtime
                    .pending_tool_request_anchor(call_id)
                    .or_else(|_| runtime.tool_request_anchor_from_raw_items(call_id, raw_items))
            })
            .collect::<Result<Vec<_>, SpineError>>()?;
        let completed_toolcall = completed_toolcall_evidence_from_segments(
            commit_call_id,
            tool_call_ids,
            completed_toolcall_request_segments(
                request_anchors
                    .iter()
                    .map(|anchor| (anchor.raw_ordinal, anchor.context_index)),
            ),
            completed_toolcall_response_segments(response_raw_ordinals, response_context_start),
            "completed grouped toolcall must contain at least one request",
            "completed grouped toolcall must contain at least one response",
        )?;
        Ok(Some(SpineToolcallCommitEvidence::new(
            commit_call_id,
            completed_toolcall,
        )))
    }

    pub(in crate::spine) fn completed_toolcall_commit_evidence_from_output(
        &self,
        output: &SpineCompletedToolCallOutputEvidence<'_>,
        output_raw_ordinals: &[Option<u64>],
        output_context_start: usize,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<Option<SpineToolcallCommitEvidence>, SpineError> {
        let evidence = match &output.request_call_ids {
            SpineCompletedToolCallRequestIds::Single(call_id) => self
                .single_completed_toolcall_evidence(
                    call_id,
                    (
                        output_raw_ordinals
                            .first()
                            .copied()
                            .flatten()
                            .ok_or_else(|| {
                                SpineError::InvalidEvent(
                                    "single Spine toolcall output missing raw ordinal".to_string(),
                                )
                            })?,
                        output_context_start,
                    ),
                    raw_items,
                ),
            SpineCompletedToolCallRequestIds::Grouped(tool_call_ids) => self
                .grouped_completed_toolcall_evidence(
                    output.call_id(),
                    tool_call_ids,
                    output_raw_ordinals,
                    output_context_start,
                    raw_items,
                ),
        }?;
        Ok(evidence.map(|evidence| evidence.with_control_policy(output.control_policy)))
    }
}
