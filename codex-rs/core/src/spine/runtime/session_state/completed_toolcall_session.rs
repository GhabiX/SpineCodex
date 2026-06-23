use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::TurnContextItem;
use codex_protocol::spine_tree::SpineTreeUpdateEvent;

use super::super::CompletedToolCall;
use super::super::SpineError;
use super::super::SpineHistoryUpdate;
use super::super::SpineHostEffects;
use super::super::SpineOpenNodeContextProjection;
use super::super::SpineTreeUpdateDelivery;
use super::super::prepared::SpineCommitPublication;
use super::super::support::is_non_toolcall_msg;
use super::super::support::is_real_user_message;
use super::super::support::tool_request_call_id;
use super::super::support::tool_response_call_id;
use super::super::types::SpinePreparedCloseMemory;
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
use super::state_types::CommittedSpineToolcall;
use super::state_types::SpineGroupedToolcallOutputRecordingPlan;
use super::state_types::SpineMessageEvidence;
use super::state_types::SpineObservedContextItem;
use super::state_types::SpinePostApplyEffectPolicy;
use super::state_types::SpineSingleToolcallOutputRecordingPlan;
use super::toolcall_host_commit::SpineToolcallCommitPreparation;

pub(crate) struct PreparedSpineToolcallCommit {
    publication: SpineCommitPublication<SpineHistoryUpdate>,
}

pub(crate) struct SpineCommitAttempt {
    pub(super) kind: SpineCommitAttemptKind,
}

pub(super) enum SpineCommitAttemptKind {
    Done(SpineHostEffects),
    Retry,
    RuntimeMissing,
}

impl SpineCommitAttempt {
    pub(crate) fn host_lock_busy() -> Self {
        Self {
            kind: SpineCommitAttemptKind::Retry,
        }
    }

    fn runtime_missing() -> Self {
        Self {
            kind: SpineCommitAttemptKind::RuntimeMissing,
        }
    }

    fn done(
        state: &mut SpineSessionState,
        committed: CommittedSpineToolcall,
        snapshot: Option<SpineTreeUpdateEvent>,
    ) -> Self {
        Self {
            kind: SpineCommitAttemptKind::Done(
                state.committed_toolcall_post_apply_host_effects(committed, snapshot),
            ),
        }
    }
}

struct SpineToolcallCommitInput<'a> {
    call_id: &'a str,
    completed_toolcall: CompletedToolCall,
    tool_resp_item: &'a ResponseItem,
    tool_resp_already_recorded: bool,
    raw_items: &'a [Option<ResponseItem>],
    history_items: &'a [ResponseItem],
    expected_history: Vec<ResponseItem>,
    reference_context_item: Option<TurnContextItem>,
    pre_compact_provider_input_tokens: Option<i64>,
    current_turn_provider_input_tokens: Option<i64>,
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
    ) -> Result<Option<super::SpineToolcallCommitHostPlan>, SpineError> {
        self.ensure_valid()?;
        let Some(runtime) = self.runtime_mut() else {
            return Ok(None);
        };
        let call_id = evidence.commit_evidence.call_id.as_str();
        let force_ordinary = evidence.commit_evidence.force_ordinary();
        if !force_ordinary {
            runtime.ensure_pending_from_toolcall_request(call_id, evidence.raw_items)?;
        }
        let preparation = SpineToolcallCommitPreparation::new(
            !force_ordinary
                && runtime.has_close_like_control_request(call_id, evidence.raw_items)?,
        );
        Ok(Some(preparation.host_plan(
            evidence.current_turn_provider_input_tokens,
            evidence.tool_resp_already_recorded,
            evidence.recorded_inside_reduce,
        )))
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
            if tool_request_call_id(item.item).is_some() {
                runtime.observe_toolcall_request_anchor(
                    item.raw_ordinal,
                    item.context_index,
                    item.item,
                )?;
            } else if let Some(call_id) = tool_response_call_id(item.item) {
                recorded_tool_outputs.push((
                    call_id.to_string(),
                    item.raw_ordinal,
                    item.context_index,
                ));
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
                continue;
            } else {
                return Err(SpineError::InvalidEvent(
                    "toolcall context observer received non-toolcall item".to_string(),
                ));
            }
        }
        if !recorded_tool_outputs.is_empty() {
            runtime.observe_recorded_tool_output_group_as_completed_toolcall_with_raw_items(
                &recorded_tool_outputs,
                raw_items,
            )?;
        }
        Ok(())
    }

    pub(crate) fn observe_non_toolcall_msg(
        &mut self,
        evidence: SpineMessageEvidence<'_>,
    ) -> Result<bool, SpineError> {
        self.ensure_valid()?;
        let Some(runtime) = self.runtime_mut() else {
            return Ok(false);
        };
        if !is_non_toolcall_msg(evidence.item) {
            return Err(SpineError::InvalidEvent(
                "on_non_toolcall_msg received toolcall item".to_string(),
            ));
        }
        let observed_user_message = is_real_user_message(evidence.item);
        if runtime.jit_enabled() && observed_user_message {
            runtime.checkpoint_before_user_msg(
                evidence.rollout_path,
                evidence.raw_ordinal,
                evidence.raw_items,
            )?;
        }
        runtime.on_non_toolcall_msg(evidence.raw_ordinal, evidence.context_index, evidence.item)?;
        Ok(observed_user_message)
    }

    pub(crate) fn observe_non_toolcall_msg_with_host_effects(
        &mut self,
        evidence: SpineMessageEvidence<'_>,
    ) -> Result<SpineHostEffects, SpineError> {
        let observed_user_message = self.observe_non_toolcall_msg(evidence)?;
        if !observed_user_message {
            return Ok(SpineHostEffects::none());
        }
        Ok(SpineHostEffects::publish_materialized_history_after_batch())
    }

    pub(crate) fn materialized_history_host_effects_if_no_pending_tool_request(
        &self,
        raw_items: &[Option<ResponseItem>],
        expected_history: Vec<ResponseItem>,
        reference_context_item: Option<TurnContextItem>,
    ) -> Result<SpineHostEffects, SpineError> {
        let Some(replacement) = self.materialize_history_if_no_pending_tool_request(raw_items)?
        else {
            return Ok(SpineHostEffects::none());
        };
        if replacement == expected_history {
            return Ok(SpineHostEffects::none());
        }
        Ok(SpineHostEffects::replace_history(SpineHistoryUpdate {
            call_id: "non-toolcall-msg".to_string(),
            operation: "publish Spine h(PS) after non-toolcall message",
            suffix_start: 0,
            expected_history,
            replacement,
            reference_context_item,
        }))
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

    fn prepare_completed_toolcall_commit(
        &mut self,
        evidence: SpineToolcallCommitEvidence,
        tool_resp_item: &ResponseItem,
        tool_resp_already_recorded: bool,
        raw_items: &[Option<ResponseItem>],
        history_items: &[ResponseItem],
        expected_history: Vec<ResponseItem>,
        reference_context_item: Option<TurnContextItem>,
        pre_compact_provider_input_tokens: Option<i64>,
        current_turn_provider_input_tokens: Option<i64>,
    ) -> Result<Option<PreparedSpineToolcallCommit>, SpineError> {
        let call_id = evidence.call_id;
        let completed_toolcall = evidence.completed_toolcall;
        let toolcall_start = completed_toolcall.first_segment_context_index()?;
        let input = SpineToolcallCommitInput {
            call_id: &call_id,
            completed_toolcall: completed_toolcall.into_completed_toolcall(),
            tool_resp_item,
            tool_resp_already_recorded,
            raw_items,
            history_items,
            expected_history,
            reference_context_item,
            pre_compact_provider_input_tokens,
            current_turn_provider_input_tokens,
        };
        self.ensure_valid()?;
        if self.runtime().is_none() {
            return Ok(None);
        }
        let memory = {
            let assembly = self
                .runtime_mut()
                .ok_or_else(|| {
                    SpineError::InvalidStore(
                        "spine runtime missing before completed toolcall commit".to_string(),
                    )
                })?
                .prepare_close_memory_assembly_for_completed_toolcall(
                    input.history_items,
                    toolcall_start,
                    input.call_id,
                );
            match assembly {
                Ok(Some(assembly)) => Some(SpinePreparedCloseMemory::new(
                    assembly,
                    input.expected_history,
                )),
                Ok(None) => None,
                Err(err) => {
                    let reason = "spine close memory assembly failed before commit";
                    if input.tool_resp_already_recorded {
                        self.invalidate(format!("{reason} for call_id={}", input.call_id));
                    } else if let Some(runtime) = self.runtime_mut() {
                        runtime.abort_pending(input.call_id);
                    }
                    return Err(err);
                }
            }
        };
        if let Some(prepared_memory) = memory.as_ref()
            && input.history_items != prepared_memory.expected_history()
        {
            let reason = "spine close history changed before commit";
            if input.tool_resp_already_recorded {
                self.invalidate(format!("{reason} for call_id={}", input.call_id));
            } else if let Some(runtime) = self.runtime_mut() {
                runtime.abort_pending(input.call_id);
            }
            return Err(SpineError::Operation(format!(
                "spine.close history changed while compacting suffix for call_id={}",
                input.call_id
            )));
        }
        let Some(runtime) = self.runtime_mut() else {
            return Ok(None);
        };
        runtime.validate_close_expected_history_for_commit(
            input.call_id,
            memory
                .as_ref()
                .map(SpinePreparedCloseMemory::expected_history),
            input.history_items,
        )?;
        let memory_assembly = memory.map(SpinePreparedCloseMemory::into_assembly);
        let commit_application = runtime
            .prepare_or_observe_completed_toolcall_with_pending_baselines(
                input.call_id,
                memory_assembly,
                input.pre_compact_provider_input_tokens,
                input.current_turn_provider_input_tokens,
                input.completed_toolcall,
                input.raw_items,
            )?;
        runtime
            .prepare_commit_publication(
                input.call_id,
                commit_application,
                input.tool_resp_item,
                input.tool_resp_already_recorded,
                input.raw_items,
                input.history_items,
                |call_id, operation, suffix_start, expected_history, replacement| {
                    SpineHistoryUpdate {
                        call_id: call_id.to_string(),
                        operation,
                        suffix_start,
                        expected_history,
                        replacement,
                        reference_context_item: input.reference_context_item,
                    }
                },
            )
            .map(PreparedSpineToolcallCommit::new)
            .map(Some)
    }

    fn persist_toolcall_commit_side_effects(
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

    fn apply_toolcall_commit(
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

    fn commit_prepared_toolcall_with_host_effects(
        &mut self,
        call_id: &str,
        mut prepared: PreparedSpineToolcallCommit,
        apply_host_effects: impl FnOnce(SpineHostEffects) -> Result<(), String>,
    ) -> Result<CommittedSpineToolcall, SpineError> {
        let host_effects = prepared.take_pre_apply_host_effects();
        let post_apply_effect_policy = prepared.post_apply_effect_policy();
        if let Err(err) = self.persist_toolcall_commit_side_effects(&prepared) {
            self.invalidate(format!(
                "failed to persist Spine prepared side effects before publishing h(PS) for call_id={call_id}: {err}"
            ));
            return Err(err);
        }
        if let Err(err) = apply_host_effects(host_effects) {
            self.invalidate(format!(
                "failed to publish Spine h(PS) before installing reduced parse stack for call_id={call_id}: {err}"
            ));
            return Err(SpineError::Invariant(err));
        }
        let installed_commit = self.apply_toolcall_commit(prepared)?;
        Ok(CommittedSpineToolcall {
            installed_commit,
            post_apply_effect_policy,
        })
    }

    pub(crate) fn attempt_completed_toolcall_commit_with_host_effects(
        &mut self,
        evidence: SpineToolcallCommitEvidence,
        tool_resp_item: &ResponseItem,
        tool_resp_already_recorded: bool,
        raw_items: &[Option<ResponseItem>],
        history_items: &[ResponseItem],
        expected_history: Vec<ResponseItem>,
        reference_context_item: Option<TurnContextItem>,
        pre_compact_provider_input_tokens: Option<i64>,
        current_turn_provider_input_tokens: Option<i64>,
        apply_host_effects: impl FnOnce(SpineHostEffects) -> Result<(), String>,
        build_snapshot: impl FnOnce(
            Option<(SpineTreeUpdateEvent, Vec<SpineOpenNodeContextProjection>)>,
        ) -> Result<Option<SpineTreeUpdateEvent>, SpineError>,
    ) -> Result<SpineCommitAttempt, SpineError> {
        let call_id = evidence.call_id.clone();
        let Some(prepared_commit) = self.prepare_completed_toolcall_commit(
            evidence,
            tool_resp_item,
            tool_resp_already_recorded,
            raw_items,
            history_items,
            expected_history,
            reference_context_item,
            pre_compact_provider_input_tokens,
            current_turn_provider_input_tokens,
        )?
        else {
            return Ok(SpineCommitAttempt::runtime_missing());
        };
        let committed = self.commit_prepared_toolcall_with_host_effects(
            &call_id,
            prepared_commit,
            apply_host_effects,
        )?;
        let projection = self.committed_toolcall_tree_snapshot_projection(&committed)?;
        let snapshot = build_snapshot(projection)?;
        Ok(SpineCommitAttempt::done(self, committed, snapshot))
    }
}
