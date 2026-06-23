use codex_protocol::models::ResponseItem;

use super::super::SpineError;
use super::super::SpineHostEffects;
use super::super::SpineRuntime;
use super::super::support::tool_request_call_id;
use super::super::support::tool_response_call_id;
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
use super::state_types::SpineObservedContextItem;
use super::state_types::SpineSingleToolcallOutputRecordingPlan;
use super::toolcall_host_commit::SpineToolcallCommitPreparation;

fn observe_toolcall_context_item(
    runtime: &mut SpineRuntime,
    item: &SpineObservedContextItem<'_>,
    recorded_tool_outputs: &mut Vec<(String, u64, usize)>,
) -> Result<(), SpineError> {
    if tool_request_call_id(item.item).is_some() {
        runtime.observe_toolcall_request_anchor(item.raw_ordinal, item.context_index, item.item)?;
    } else if let Some(call_id) = tool_response_call_id(item.item) {
        recorded_tool_outputs.push((call_id.to_string(), item.raw_ordinal, item.context_index));
        runtime.observe_toolcall_response_anchor(
            item.raw_ordinal,
            item.context_index,
            item.item,
        )?;
    } else if matches!(
        item.item,
        ResponseItem::ToolSearchOutput { call_id: None, .. }
            | ResponseItem::ToolSearchCall { call_id: None, .. }
    ) {
        return Ok(());
    } else {
        return Err(SpineError::InvalidEvent(
            "toolcall context observer received non-toolcall item".to_string(),
        ));
    }
    Ok(())
}

fn single_output_raw_ordinal(output_raw_ordinals: &[Option<u64>]) -> Result<u64, SpineError> {
    output_raw_ordinals
        .first()
        .copied()
        .flatten()
        .ok_or_else(|| {
            SpineError::InvalidEvent("single Spine toolcall output missing raw ordinal".to_string())
        })
}

fn prepare_toolcall_commit_preparation(
    runtime: &mut SpineRuntime,
    call_id: &str,
    raw_items: &[Option<ResponseItem>],
    force_ordinary: bool,
) -> Result<SpineToolcallCommitPreparation, SpineError> {
    if !force_ordinary {
        runtime.ensure_pending_from_toolcall_request(call_id, raw_items)?;
    }
    Ok(SpineToolcallCommitPreparation::new(
        !force_ordinary && runtime.has_close_like_control_request(call_id, raw_items)?,
    ))
}

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

    pub(crate) fn is_control_output_call_id(&self, call_id: &str) -> bool {
        let Some(runtime) = self.runtime() else {
            return false;
        };
        runtime.is_control_output_call_id(call_id)
    }

    pub(crate) fn prepare_completed_toolcall_for_commit(
        &mut self,
        evidence: SpineToolcallHookEvidence<'_>,
    ) -> Result<SpineHostEffects, SpineError> {
        self.ensure_valid()?;
        let Some(runtime) = self.runtime_mut() else {
            return Ok(SpineHostEffects::none());
        };
        let call_id = evidence.commit_evidence.call_id.as_str();
        let force_ordinary = evidence.commit_evidence.force_ordinary();
        let preparation = prepare_toolcall_commit_preparation(
            runtime,
            call_id,
            evidence.raw_items,
            force_ordinary,
        )?;
        let plan = preparation.host_plan(
            evidence.current_turn_provider_input_tokens,
            evidence.tool_resp_already_recorded,
            evidence.recorded_inside_reduce,
        );
        Ok(SpineHostEffects::toolcall_host_commit(
            plan.into_host_commit(),
        ))
    }

    pub(crate) fn observe_toolcall_context_items(
        &mut self,
        items: &[SpineObservedContextItem<'_>],
        raw_items: &[Option<ResponseItem>],
    ) -> Result<(), SpineError> {
        self.ensure_valid()?;
        let Some(runtime) = self.runtime_mut() else {
            return Ok(());
        };
        let mut recorded_tool_outputs = Vec::<(String, u64, usize)>::new();
        for item in items {
            observe_toolcall_context_item(runtime, item, &mut recorded_tool_outputs)?;
        }
        if !recorded_tool_outputs.is_empty() {
            runtime.observe_recorded_tool_output_group_as_completed_toolcall_with_raw_items(
                &recorded_tool_outputs,
                raw_items,
            )?;
        }
        Ok(())
    }

    pub(crate) fn single_completed_toolcall_evidence(
        &self,
        call_id: &str,
        response_anchor: (u64, usize),
    ) -> Result<Option<SpineToolcallCommitEvidence>, SpineError> {
        self.ensure_valid()?;
        let Some(runtime) = self.runtime() else {
            return Ok(None);
        };
        let request_anchor = runtime.pending_tool_request_anchor(call_id)?;
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

    pub(crate) fn grouped_completed_toolcall_evidence(
        &self,
        commit_call_id: &str,
        tool_call_ids: &[String],
        response_raw_ordinals: &[Option<u64>],
        response_context_start: usize,
    ) -> Result<Option<SpineToolcallCommitEvidence>, SpineError> {
        self.ensure_valid()?;
        let Some(runtime) = self.runtime() else {
            return Ok(None);
        };
        let request_anchors = tool_call_ids
            .iter()
            .map(|call_id| runtime.pending_tool_request_anchor(call_id))
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

    pub(crate) fn completed_toolcall_commit_evidence_from_output(
        &self,
        output: &SpineCompletedToolCallOutputEvidence<'_>,
        output_raw_ordinals: &[Option<u64>],
        output_context_start: usize,
    ) -> Result<Option<SpineToolcallCommitEvidence>, SpineError> {
        let evidence = match &output.request_call_ids {
            SpineCompletedToolCallRequestIds::Single(call_id) => self
                .single_completed_toolcall_evidence(
                    call_id,
                    (
                        single_output_raw_ordinal(output_raw_ordinals)?,
                        output_context_start,
                    ),
                ),
            SpineCompletedToolCallRequestIds::Grouped(tool_call_ids) => self
                .grouped_completed_toolcall_evidence(
                    output.call_id(),
                    tool_call_ids,
                    output_raw_ordinals,
                    output_context_start,
                ),
        }?;
        Ok(evidence.map(|evidence| evidence.with_control_policy(output.control_policy)))
    }
}
