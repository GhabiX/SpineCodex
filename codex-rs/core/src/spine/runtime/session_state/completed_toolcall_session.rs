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
use super::super::types::SpinePreparedCloseMemory;
use super::SpineSessionState;
use super::completed_toolcall_evidence::SpineToolCallControlPolicy;
use super::completed_toolcall_evidence::SpineToolcallCommitEvidence;
use super::state_types::CommittedSpineToolcall;
use super::toolcall_host_commit::SpineToolcallHostAttempt;
use crate::spine::model::TrimBodyUpdate;

pub(crate) struct PreparedSpineToolcallCommit {
    publication: SpineCommitPublication<SpineHistoryUpdate>,
}

pub(super) enum SpineCommitAttemptKind {
    Done(SpineHostEffects),
    Retry,
    RuntimeMissing,
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

enum SpineToolcallCommitPreparation {
    Prepared(PreparedSpineToolcallCommit),
    NoSpineCommit {
        trim_body_updates: Vec<TrimBodyUpdate>,
    },
}

impl SpineSessionState {
    fn handle_close_precommit_failure(
        &mut self,
        call_id: &str,
        tool_resp_already_recorded: bool,
        reason: &'static str,
    ) {
        if tool_resp_already_recorded {
            self.invalidate(format!("{reason} for call_id={call_id}"));
        } else if let Some(runtime) = self.runtime_mut() {
            runtime.abort_pending(call_id);
        }
    }

    fn prepare_completed_toolcall_close_memory(
        &mut self,
        input: &SpineToolcallCommitInput<'_>,
        toolcall_start: usize,
    ) -> Result<Option<SpinePreparedCloseMemory>, SpineError> {
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
            Ok(Some(assembly)) => Ok(Some(SpinePreparedCloseMemory::new(
                assembly,
                input.expected_history.clone(),
            ))),
            Ok(None) => Ok(None),
            Err(err) => {
                self.handle_close_precommit_failure(
                    input.call_id,
                    input.tool_resp_already_recorded,
                    "spine close memory assembly failed before commit",
                );
                Err(err)
            }
        }
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
    ) -> Result<Option<SpineToolcallCommitPreparation>, SpineError> {
        let force_ordinary = evidence.control_policy == SpineToolCallControlPolicy::ForceOrdinary;
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
        if force_ordinary {
            let Some(runtime) = self.runtime_mut() else {
                return Ok(None);
            };
            let (_, trim_body_updates) = runtime
                .commit_completed_toolcall_as_ordinary_with_raw_items(
                    input.call_id,
                    input.completed_toolcall,
                    input.raw_items,
                )?;
            return Ok(Some(SpineToolcallCommitPreparation::NoSpineCommit {
                trim_body_updates,
            }));
        }
        let memory = self.prepare_completed_toolcall_close_memory(&input, toolcall_start)?;
        if let Some(prepared_memory) = memory.as_ref()
            && input.history_items != prepared_memory.expected_history()
        {
            self.handle_close_precommit_failure(
                input.call_id,
                input.tool_resp_already_recorded,
                "spine close history changed before commit",
            );
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
        let (commit_application, observed_trim_body_updates) = runtime
            .prepare_or_observe_completed_toolcall_with_pending_baselines(
                input.call_id,
                memory_assembly,
                input.pre_compact_provider_input_tokens,
                input.current_turn_provider_input_tokens,
                input.completed_toolcall,
                input.raw_items,
            )?;
        let Some(commit_application) = commit_application else {
            return Ok(Some(SpineToolcallCommitPreparation::NoSpineCommit {
                trim_body_updates: observed_trim_body_updates,
            }));
        };
        runtime
            .prepare_commit_publication(
                input.call_id,
                Some(commit_application),
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
            .map(|publication| PreparedSpineToolcallCommit { publication })
            .map(SpineToolcallCommitPreparation::Prepared)
            .map(Some)
    }

    fn persist_toolcall_commit_side_effects(
        &mut self,
        prepared: &PreparedSpineToolcallCommit,
    ) -> Result<Vec<TrimBodyUpdate>, SpineError> {
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
        let host_effects = match prepared.publication.take_pre_apply_host_history_update() {
            Some(update) => SpineHostEffects::replace_history(update),
            None => SpineHostEffects::none(),
        };
        let delivery = if prepared.publication.defer_tree_update_until_raw_output() {
            SpineTreeUpdateDelivery::AfterRawOutputDurable
        } else {
            SpineTreeUpdateDelivery::Immediate
        };
        let trim_body_updates = match self.persist_toolcall_commit_side_effects(&prepared) {
            Ok(updates) => updates,
            Err(err) => {
                self.invalidate(format!(
                "failed to persist Spine prepared side effects before publishing h(PS) for call_id={call_id}: {err}"
            ));
                return Err(err);
            }
        };
        if let Err(err) = apply_host_effects(host_effects) {
            self.invalidate(format!(
                "failed to publish Spine h(PS) before installing reduced parse stack for call_id={call_id}: {err}"
            ));
            return Err(SpineError::Invariant(err));
        }
        let installed_commit = self.apply_toolcall_commit(prepared)?;
        Ok(CommittedSpineToolcall {
            installed_commit,
            delivery,
            trim_body_updates,
        })
    }

    pub(in crate::spine) fn attempt_completed_toolcall_commit_with_host_effects(
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
    ) -> Result<SpineToolcallHostAttempt, SpineError> {
        let call_id = evidence.call_id.clone();
        let Some(preparation) = self.prepare_completed_toolcall_commit(
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
            return Ok(SpineToolcallHostAttempt::runtime_missing());
        };
        match preparation {
            SpineToolcallCommitPreparation::Prepared(prepared_commit) => {
                let committed = self.commit_prepared_toolcall_with_host_effects(
                    &call_id,
                    prepared_commit,
                    apply_host_effects,
                )?;
                let projection = if committed.installed_commit {
                    self.tree_snapshot_projection()?
                } else {
                    None
                };
                let snapshot = build_snapshot(projection)?;
                let tree_effects = match snapshot {
                    Some(snapshot) => SpineHostEffects::tree_update(snapshot, committed.delivery),
                    None => SpineHostEffects::none(),
                };
                let post_apply_host_effects = tree_effects.combine(
                    SpineHostEffects::trim_body_updates(committed.trim_body_updates),
                );
                Ok(SpineToolcallHostAttempt::done(post_apply_host_effects))
            }
            SpineToolcallCommitPreparation::NoSpineCommit { trim_body_updates } => {
                Ok(SpineToolcallHostAttempt::done(
                    SpineHostEffects::trim_body_updates(trim_body_updates),
                ))
            }
        }
    }
}
