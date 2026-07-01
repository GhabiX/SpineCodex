use codex_protocol::models::ResponseItem;

use super::super::CompletedToolCallSegment;
use super::super::SpineError;
use super::super::SpineHostEffects;
use super::super::ToolRequestAnchor;
use super::SpineSessionState;
use super::completed_toolcall_evidence::SpineCompletedToolCallOutputEvidence;
use super::completed_toolcall_evidence::SpineCompletedToolCallRequestIds;
use super::completed_toolcall_evidence::SpineToolCallControlPolicy;
use super::completed_toolcall_evidence::SpineToolcallCommitEvidence;
use super::completed_toolcall_evidence::SpineToolcallHookEvidence;
use super::completed_toolcall_evidence::assign_response_item_raw_ordinals;
use super::completed_toolcall_evidence::completed_toolcall_evidence_from_segments;
use super::completed_toolcall_evidence::completed_toolcall_request_segments;
use super::completed_toolcall_evidence::completed_toolcall_response_segments;
use super::state_types::SpineGroupedToolcallOutputRecordingPlan;
use super::state_types::SpineSingleToolcallOutputRecordingPlan;
use super::toolcall_host_commit::SpineToolcallCommitHostPlan;
use crate::spine::model::ToolCallSegmentKind;

impl SpineSessionState {
    pub(in crate::spine) fn prepare_single_toolcall_output_recording(
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

    pub(in crate::spine) fn prepare_grouped_toolcall_output_recording(
        &self,
        output_items: &[ResponseItem],
    ) -> Result<SpineGroupedToolcallOutputRecordingPlan, SpineError> {
        self.ensure_valid()?;
        Ok(SpineGroupedToolcallOutputRecordingPlan {
            raw_ordinals: assign_response_item_raw_ordinals(self.raw_len, output_items)?,
        })
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
            evidence.output_context_indices,
            evidence.raw_items,
        )?
        else {
            return Ok(SpineHostEffects::none());
        };
        let Some(runtime) = self.runtime_mut() else {
            return Ok(SpineHostEffects::none());
        };
        let call_id = &commit_evidence.call_id;
        let force_ordinary =
            commit_evidence.control_policy == SpineToolCallControlPolicy::ForceOrdinary;
        if !force_ordinary {
            runtime.ensure_pending_from_toolcall_request(call_id, evidence.raw_items)?;
        }
        let has_close_like_control = !force_ordinary
            && runtime.has_close_like_control_request(call_id, evidence.raw_items)?;
        let plan = SpineToolcallCommitHostPlan::new(
            has_close_like_control,
            evidence.current_turn_provider_input_tokens,
            evidence.tool_resp_already_recorded,
            evidence.recorded_inside_reduce,
        );
        Ok(SpineHostEffects::toolcall_host_commit(
            plan.into_host_commit(commit_evidence),
        ))
    }

    fn single_completed_toolcall_evidence(
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
            vec![CompletedToolCallSegment {
                kind: ToolCallSegmentKind::Request,
                raw_ordinal: request_anchor.raw_ordinal,
                context_index: request_anchor.context_index,
            }],
            vec![CompletedToolCallSegment {
                kind: ToolCallSegmentKind::Response,
                raw_ordinal: response_anchor.0,
                context_index: response_anchor.1,
            }],
            "completed toolcall must contain a request",
            "completed toolcall must contain a response",
        )?;
        Ok(Some(SpineToolcallCommitEvidence::new(
            call_id,
            completed_toolcall,
        )))
    }

    fn grouped_completed_toolcall_evidence(
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
        validate_grouped_toolcall_mutable_context_slots(
            response_raw_ordinals,
            response_context_start,
            &request_anchors,
        )?;
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

    fn grouped_completed_toolcall_evidence_with_response_context_indices(
        &self,
        commit_call_id: &str,
        tool_call_ids: &[String],
        response_raw_ordinals: &[Option<u64>],
        response_context_indices: &[usize],
        raw_items: &[Option<ResponseItem>],
    ) -> Result<Option<SpineToolcallCommitEvidence>, SpineError> {
        self.ensure_valid()?;
        let Some(runtime) = self.runtime() else {
            return Ok(None);
        };
        if response_raw_ordinals.len() != response_context_indices.len() {
            return Err(SpineError::InvalidEvent(format!(
                "grouped Spine toolcall has {} response raw ordinals but {} response context indices",
                response_raw_ordinals.len(),
                response_context_indices.len()
            )));
        }
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
            completed_toolcall_response_segments_from_indices(
                response_raw_ordinals,
                response_context_indices,
            ),
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
        output_context_indices: Option<&[usize]>,
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
            SpineCompletedToolCallRequestIds::Grouped(tool_call_ids) => {
                if let Some(response_context_indices) = output_context_indices {
                    self.grouped_completed_toolcall_evidence_with_response_context_indices(
                        output.call_id(),
                        tool_call_ids,
                        output_raw_ordinals,
                        response_context_indices,
                        raw_items,
                    )
                } else {
                    self.grouped_completed_toolcall_evidence(
                        output.call_id(),
                        tool_call_ids,
                        output_raw_ordinals,
                        output_context_start,
                        raw_items,
                    )
                }
            }
        }?;
        Ok(evidence.map(|mut evidence| {
            evidence.control_policy = output.control_policy;
            evidence
        }))
    }
}

fn completed_toolcall_response_segments_from_indices(
    response_raw_ordinals: &[Option<u64>],
    response_context_indices: &[usize],
) -> Vec<super::super::CompletedToolCallSegment> {
    response_raw_ordinals
        .iter()
        .zip(response_context_indices.iter().copied())
        .filter_map(|(raw_ordinal, context_index)| {
            raw_ordinal.map(|raw_ordinal| CompletedToolCallSegment {
                kind: ToolCallSegmentKind::Response,
                raw_ordinal,
                context_index,
            })
        })
        .collect()
}

fn validate_grouped_toolcall_mutable_context_slots(
    response_raw_ordinals: &[Option<u64>],
    response_context_start: usize,
    request_anchors: &[ToolRequestAnchor],
) -> Result<(), SpineError> {
    let Some(last_request_context_index) = request_anchors
        .iter()
        .map(|anchor| anchor.context_index)
        .max()
    else {
        return Ok(());
    };
    let current_mutable_len_before_output =
        last_request_context_index.checked_add(1).ok_or_else(|| {
            SpineError::InvalidEvent("grouped toolcall context index overflow".to_string())
        })?;
    if response_context_start == current_mutable_len_before_output {
        return Ok(());
    }
    let raw_ordinals = request_anchors
        .iter()
        .map(|anchor| Some(anchor.raw_ordinal))
        .chain(response_raw_ordinals.iter().copied())
        .collect::<Vec<_>>();
    let request_context_indices = request_anchors
        .iter()
        .map(|anchor| anchor.context_index)
        .collect::<Vec<_>>();
    let response_context_indices = response_raw_ordinals
        .iter()
        .enumerate()
        .filter_map(|(offset, raw_ordinal)| {
            raw_ordinal.map(|_| response_context_start.saturating_add(offset))
        })
        .collect::<Vec<_>>();
    Err(SpineError::InvalidEvent(format!(
        "grouped Spine toolcall mixes mutable context coordinates: raw_ordinal list={raw_ordinals:?}, request ctx list={request_context_indices:?}, response ctx list={response_context_indices:?}, computed output_context_start={response_context_start}, current mutable len={current_mutable_len_before_output}",
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grouped_toolcall_rejects_mixed_mutable_context_coordinates() {
        let err = validate_grouped_toolcall_mutable_context_slots(
            &[Some(50), Some(51)],
            48,
            &[
                ToolRequestAnchor {
                    raw_ordinal: 48,
                    context_index: 8,
                },
                ToolRequestAnchor {
                    raw_ordinal: 49,
                    context_index: 9,
                },
            ],
        )
        .expect_err("raw-prefix response coordinates must be rejected");
        let err = err.to_string();
        assert!(
            err.contains("raw_ordinal list=[Some(48), Some(49), Some(50), Some(51)]")
                && err.contains("request ctx list=[8, 9]")
                && err.contains("response ctx list=[48, 49]")
                && err.contains("computed output_context_start=48")
                && err.contains("current mutable len=10"),
            "mixed-coordinate diagnostic must include index evidence: {err}"
        );
    }

    #[test]
    fn grouped_toolcall_accepts_current_mutable_response_boundary() {
        validate_grouped_toolcall_mutable_context_slots(
            &[Some(50), Some(51)],
            10,
            &[
                ToolRequestAnchor {
                    raw_ordinal: 48,
                    context_index: 8,
                },
                ToolRequestAnchor {
                    raw_ordinal: 49,
                    context_index: 9,
                },
            ],
        )
        .expect("current mutable response boundary must be accepted");
    }
}
