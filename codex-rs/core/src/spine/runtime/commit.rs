use codex_protocol::models::ResponseItem;

use super::SpineError;
use super::SpineRuntime;
use super::close_family::CloseFamilyAfterClose;
use super::close_family::CloseFamilyOpenPlan;
use super::close_family::CloseFamilyPlan;
use super::close_family::CloseFamilyTransaction;
use super::close_family::CloseFamilyTransactionError;
use super::close_family::PreparedCloseCommit;
use super::pending::CompletedToolCall;
#[cfg(test)]
use super::pending::CompletedToolCallSegment;
use super::pending::PendingTransition;
use super::prepared::HistoryPublicationPlan;
use super::prepared::SpineCommitKind;
use super::prepared::SpineCommitPublication;
use super::prepared::SpinePreparedCommit;
use super::prepared::SpinePreparedCommitApplication;
use super::support::close_commit_marker;
use super::support::close_event_boundary;
use super::support::completed_toolcall_first_segment;
use super::types::SpineCloseMemoryAssembly;
use super::types::SpinePendingCommit;
use super::types::SpineTokenBaselines;
use crate::spine::archive::SpineArchive;
use crate::spine::archive::flush_archive_writes;
use crate::spine::archive::memory_ref;
use crate::spine::lexer::ControlIntent;
use crate::spine::lexer::LexedTokenKind;
use crate::spine::lexer::plan_control_toolcall;
use crate::spine::model::ContextBaselineSource;
use crate::spine::model::SpineCommitKindMarker;
#[cfg(test)]
use crate::spine::model::ToolCallSegmentKind;
use crate::spine::parse_stack::ParseStack;
use crate::spine::parse_stack::PreparedTaskTreeReduction;
use crate::spine::render::memory_response_item;

impl SpineRuntime {
    #[cfg(test)]
    pub(crate) fn maybe_commit_output(
        &mut self,
        call_id: &str,
        memory_assembly: Option<SpineCloseMemoryAssembly>,
    ) -> Result<Option<SpineCommitKind>, SpineError> {
        let completed_toolcall = self.observed_completed_toolcall(call_id)?;
        self.maybe_commit_output_impl(
            call_id,
            memory_assembly,
            SpineTokenBaselines::default(),
            completed_toolcall,
            &[],
        )
        .and_then(|prepared| self.install_prepared_commit_for_kind(prepared))
    }

    #[cfg(test)]
    pub(crate) fn maybe_commit_output_with_open_input_tokens(
        &mut self,
        call_id: &str,
        memory_assembly: Option<SpineCloseMemoryAssembly>,
        input_tokens: Option<i64>,
    ) -> Result<Option<SpineCommitKind>, SpineError> {
        let completed_toolcall = self.observed_completed_toolcall(call_id)?;
        self.maybe_commit_output_impl(
            call_id,
            memory_assembly,
            SpineTokenBaselines {
                provider_input_tokens: input_tokens,
            },
            completed_toolcall,
            &[],
        )
        .and_then(|prepared| self.install_prepared_commit_for_kind(prepared))
    }

    #[cfg(test)]
    pub(crate) fn maybe_commit_output_with_token_baselines(
        &mut self,
        call_id: &str,
        memory_assembly: Option<SpineCloseMemoryAssembly>,
        token_baselines: SpineTokenBaselines,
    ) -> Result<Option<SpineCommitKind>, SpineError> {
        let completed_toolcall = self.observed_completed_toolcall(call_id)?;
        self.maybe_commit_output_impl(
            call_id,
            memory_assembly,
            token_baselines,
            completed_toolcall,
            &[],
        )
        .and_then(|prepared| self.install_prepared_commit_for_kind(prepared))
    }

    pub(crate) fn maybe_commit_output_with_toolcall(
        &mut self,
        call_id: &str,
        memory_assembly: Option<SpineCloseMemoryAssembly>,
        token_baselines: SpineTokenBaselines,
        completed_toolcall: CompletedToolCall,
    ) -> Result<Option<SpineCommitKind>, SpineError> {
        self.maybe_commit_output_with_toolcall_and_raw_items(
            call_id,
            memory_assembly,
            token_baselines,
            completed_toolcall,
            &[],
        )
    }

    pub(crate) fn maybe_commit_output_with_toolcall_and_raw_items(
        &mut self,
        call_id: &str,
        memory_assembly: Option<SpineCloseMemoryAssembly>,
        token_baselines: SpineTokenBaselines,
        completed_toolcall: CompletedToolCall,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<Option<SpineCommitKind>, SpineError> {
        let Some(prepared) = self.prepare_commit_output_with_toolcall_and_raw_items(
            call_id,
            memory_assembly,
            token_baselines,
            completed_toolcall,
            raw_items,
        )?
        else {
            return Ok(None);
        };
        let kind = prepared.kind.clone();
        self.persist_prepared_commit_side_effects(&prepared)?;
        self.install_prepared_commit(prepared);
        Ok(Some(kind))
    }

    pub(crate) fn prepare_commit_output_with_toolcall_and_raw_items(
        &mut self,
        call_id: &str,
        memory_assembly: Option<SpineCloseMemoryAssembly>,
        token_baselines: SpineTokenBaselines,
        completed_toolcall: CompletedToolCall,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<Option<SpinePreparedCommit>, SpineError> {
        self.maybe_commit_output_impl(
            call_id,
            memory_assembly,
            token_baselines,
            Some(completed_toolcall),
            raw_items,
        )
    }

    pub(crate) fn prepare_or_observe_completed_toolcall_for_commit(
        &mut self,
        call_id: &str,
        pending_commit: Option<&SpinePendingCommit>,
        memory_assembly: Option<SpineCloseMemoryAssembly>,
        token_baselines: SpineTokenBaselines,
        completed_toolcall: CompletedToolCall,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<Option<SpinePreparedCommit>, SpineError> {
        if pending_commit.is_some() {
            self.prepare_commit_output_with_toolcall_and_raw_items(
                call_id,
                memory_assembly,
                token_baselines,
                completed_toolcall,
                raw_items,
            )
        } else {
            self.observe_completed_toolcall_with_raw_items(completed_toolcall, raw_items)?;
            Ok(None)
        }
    }

    pub(crate) fn prepare_or_observe_completed_toolcall_with_pending_baselines(
        &mut self,
        call_id: &str,
        memory_assembly: Option<SpineCloseMemoryAssembly>,
        pre_compact_provider_input_tokens: Option<i64>,
        current_turn_provider_input_tokens: Option<i64>,
        completed_toolcall: CompletedToolCall,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<Option<SpinePreparedCommitApplication>, SpineError> {
        let pending_commit = self.pending_commit(call_id)?;
        let pre_compact_token_baselines =
            pre_compact_provider_input_tokens.map(|tokens| SpineTokenBaselines {
                provider_input_tokens: Some(tokens),
            });
        let current_turn_token_baselines = SpineTokenBaselines {
            provider_input_tokens: current_turn_provider_input_tokens,
        };
        let token_baselines = match pending_commit {
            Some(SpinePendingCommit::Close { .. }) => {
                pre_compact_token_baselines.unwrap_or(current_turn_token_baselines)
            }
            Some(SpinePendingCommit::Open) => current_turn_token_baselines,
            None => SpineTokenBaselines::default(),
        };
        self.prepare_or_observe_completed_toolcall_for_commit(
            call_id,
            pending_commit.as_ref(),
            memory_assembly,
            token_baselines,
            completed_toolcall,
            raw_items,
        )
        .map(|prepared| prepared.map(SpinePreparedCommit::into_application))
    }

    pub(crate) fn validate_close_expected_history_for_commit(
        &mut self,
        call_id: &str,
        expected_history: Option<&[ResponseItem]>,
        history_items: &[ResponseItem],
    ) -> Result<(), SpineError> {
        if let Some(expected_history) = expected_history
            && history_items != expected_history
        {
            if self.abort_pending(call_id) {
                tracing::debug!(
                    call_id,
                    reason = "spine close history changed before suffix replacement",
                    "aborted pending Spine transition"
                );
            }
            return Err(SpineError::Operation(format!(
                "spine.close history changed before suffix replacement for call_id={call_id}"
            )));
        }
        Ok(())
    }

    fn maybe_commit_output_impl(
        &mut self,
        call_id: &str,
        memory_assembly: Option<SpineCloseMemoryAssembly>,
        token_baselines: SpineTokenBaselines,
        completed_toolcall: Option<CompletedToolCall>,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<Option<SpinePreparedCommit>, SpineError> {
        #[cfg(test)]
        self.ensure_pending_from_receipt(call_id)?;
        self.ensure_pending_from_toolcall_request(call_id, raw_items)?;
        let Some(pending) = self.pending.as_ref() else {
            return Ok(None);
        };
        if pending.call_id() != call_id {
            return Ok(None);
        }
        let pending = pending.clone();
        let plan = plan_control_toolcall(pending.control_intent());
        let commit_kind = match pending {
            PendingTransition::Open {
                summary,
                boundary,
                index,
                ..
            } => {
                debug_assert_eq!(plan.intent(), ControlIntent::Open);
                debug_assert_eq!(
                    plan.token_sequence(),
                    &[LexedTokenKind::Open, LexedTokenKind::ToolCall]
                );
                self.commit_open_pending(
                    summary,
                    boundary,
                    index,
                    token_baselines,
                    completed_toolcall,
                    raw_items,
                )?
            }
            PendingTransition::Close { .. } => {
                debug_assert_eq!(plan.intent(), ControlIntent::Close);
                debug_assert_eq!(
                    plan.token_sequence(),
                    &[LexedTokenKind::Close, LexedTokenKind::ToolCall]
                );
                self.commit_close_pending(
                    memory_assembly,
                    token_baselines,
                    completed_toolcall,
                    raw_items,
                )?
            }
            PendingTransition::NextSugar { summary, .. } => {
                debug_assert_eq!(plan.intent(), ControlIntent::Next);
                debug_assert_eq!(
                    plan.token_sequence(),
                    &[
                        LexedTokenKind::Close,
                        LexedTokenKind::Open,
                        LexedTokenKind::ToolCall
                    ]
                );
                self.commit_next_sugar_pending(
                    summary,
                    memory_assembly,
                    token_baselines,
                    completed_toolcall,
                    raw_items,
                )?
            }
        };
        self.pending = None;
        self.control_call_ids.remove(call_id);
        #[cfg(test)]
        self.control_receipts.remove(call_id);
        Ok(Some(commit_kind))
    }

    fn install_prepared_commit_for_kind(
        &mut self,
        prepared: Option<SpinePreparedCommit>,
    ) -> Result<Option<SpineCommitKind>, SpineError> {
        let Some(prepared) = prepared else {
            return Ok(None);
        };
        let kind = prepared.kind.clone();
        self.persist_prepared_commit_side_effects(&prepared)?;
        self.install_prepared_commit(prepared);
        Ok(Some(kind))
    }

    fn task_tree_reduced_from(
        &self,
        parse_stack: ParseStack,
        reduction: PreparedTaskTreeReduction,
    ) -> Result<ParseStack, SpineError> {
        parse_stack.task_tree_reduced(reduction)
    }

    fn commit_open_pending(
        &mut self,
        summary: String,
        mut boundary: u64,
        mut index: u64,
        token_baselines: SpineTokenBaselines,
        completed_toolcall: Option<CompletedToolCall>,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<SpinePreparedCommit, SpineError> {
        if let Some(completed_toolcall) = completed_toolcall.as_ref() {
            let first = completed_toolcall_first_segment(completed_toolcall)?;
            boundary = first.raw_ordinal;
            index = u64::try_from(first.context_index).map_err(|_| {
                SpineError::InvalidEvent(
                    "spine.open grouped toolcall context index overflow".to_string(),
                )
            })?;
        }
        let child = self.parser.next_child_id()?;
        let open_context_source = token_baselines
            .provider_input_tokens
            .map(|_| ContextBaselineSource::ProviderAtOpen);
        let (event, token) = crate::spine::lexer::lex_open_event_token(
            &self.archive(),
            child,
            boundary,
            index,
            summary,
            token_baselines.provider_input_tokens,
            token_baselines.provider_input_tokens,
            open_context_source,
        )?;
        let open_token = token;
        if let Some(completed_toolcall) = completed_toolcall {
            let (toolcall_event, token) = self.completed_toolcall_parts(&completed_toolcall)?;
            let staged_parse_stack = self
                .parser
                .staged_after_tokens([open_token, token], &self.archive())?;
            let toolcall_seq = self.ledger.next_event_seq.checked_add(1).ok_or_else(|| {
                SpineError::InvalidEvent("spine.open toolcall seq overflow".to_string())
            })?;
            let events = vec![event, toolcall_event];
            self.append_committed_events_no_marker(events)?;
            self.parser
                .replace_parse_stack_for_runtime_transition(staged_parse_stack);
            self.append_trim_candidates_for_completed_toolcall(
                &completed_toolcall,
                toolcall_seq,
                raw_items,
            )?;
            self.clear_completed_toolcall_anchors(&completed_toolcall);
            return Ok(SpinePreparedCommit {
                kind: SpineCommitKind::Open {
                    open_request_index: usize::try_from(index).map_err(|_| {
                        SpineError::InvalidEvent("spine.open context index overflow".to_string())
                    })?,
                },
                publication_plan: None,
                final_parse_stack: None,
                completed_toolcall: None,
                toolcall_seq: None,
                raw_items: Vec::new(),
                mem_for_accounting: None,
            });
        }
        let staged_parse_stack = self
            .parser
            .staged_after_tokens([open_token], &self.archive())?;
        let events = vec![event];
        self.append_committed_events_no_marker(events)?;
        self.parser
            .replace_parse_stack_for_runtime_transition(staged_parse_stack);
        Ok(SpinePreparedCommit {
            kind: SpineCommitKind::Open {
                open_request_index: usize::try_from(index).map_err(|_| {
                    SpineError::InvalidEvent("spine.open context index overflow".to_string())
                })?,
            },
            publication_plan: None,
            final_parse_stack: None,
            completed_toolcall: None,
            toolcall_seq: None,
            raw_items: Vec::new(),
            mem_for_accounting: None,
        })
    }

    fn commit_close_pending(
        &mut self,
        memory_assembly: Option<SpineCloseMemoryAssembly>,
        token_baselines: SpineTokenBaselines,
        completed_toolcall: Option<CompletedToolCall>,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<SpinePreparedCommit, SpineError> {
        self.commit_close_family_pending(
            CloseFamilyAfterClose::None,
            memory_assembly,
            token_baselines,
            completed_toolcall,
            raw_items,
        )
    }

    fn commit_next_sugar_pending(
        &mut self,
        summary: String,
        memory_assembly: Option<SpineCloseMemoryAssembly>,
        token_baselines: SpineTokenBaselines,
        completed_toolcall: Option<CompletedToolCall>,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<SpinePreparedCommit, SpineError> {
        self.commit_close_family_pending(
            CloseFamilyAfterClose::Open { summary },
            memory_assembly,
            token_baselines,
            completed_toolcall,
            raw_items,
        )
    }

    fn commit_close_family_pending(
        &mut self,
        after_close: CloseFamilyAfterClose,
        memory_assembly: Option<SpineCloseMemoryAssembly>,
        token_baselines: SpineTokenBaselines,
        completed_toolcall: Option<CompletedToolCall>,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<SpinePreparedCommit, SpineError> {
        let prepared = self.prepare_close_commit(memory_assembly, token_baselines)?;
        let plan = self.close_family_plan(&prepared, after_close)?;
        let mut events = vec![prepared.close_event.clone()];
        if let Some(open) = plan.open.as_ref() {
            events.push(open.event.clone());
        }
        let completed_toolcall = completed_toolcall
            .ok_or_else(|| SpineError::InvalidEvent(plan.missing_toolcall_error.to_string()))?;
        let toolcall_start = completed_toolcall_first_segment(&completed_toolcall)?.context_index;
        let toolcall_context_index = match plan.toolcall_context_index {
            Some(index) => index,
            None => prepared
                .suffix_start
                .checked_add(prepared.replacement.len())
                .ok_or_else(|| {
                    SpineError::InvalidEvent(
                        "spine.close toolcall context index overflow".to_string(),
                    )
                })?,
        };
        let completed_toolcall = self
            .remap_completed_toolcall_context_indices(completed_toolcall, toolcall_context_index)?;
        let (toolcall_event, token) = self.completed_toolcall_parts(&completed_toolcall)?;
        events.push(toolcall_event);
        let event_count = u64::try_from(events.len())
            .map_err(|_| SpineError::InvalidEvent("spine event count overflow".to_string()))?;
        let toolcall_seq = self
            .ledger
            .next_event_seq
            .checked_add(event_count.checked_sub(1).ok_or_else(|| {
                SpineError::InvalidEvent(plan.event_count_underflow_error.to_string())
            })?)
            .ok_or_else(|| {
                SpineError::InvalidEvent(plan.toolcall_seq_overflow_error.to_string())
            })?;
        let mut pending_close_parse_stack = self.parser.parse_stack().clone();
        pending_close_parse_stack.shift_pending_close(prepared.memory.clone(), &self.archive())?;
        let mut final_parse_stack = self.task_tree_reduced_from(
            pending_close_parse_stack.clone(),
            prepared.task_tree_reduction,
        )?;
        if let Some(open) = plan.open.as_ref() {
            let token = crate::spine::lexer::lex_open_token(
                &self.archive(),
                open.child.clone(),
                self.raw_len,
                open.open_index_u64,
                open.summary.clone(),
                None,
                None,
                None,
            )?;
            final_parse_stack.shift(token, &self.archive())?;
        }
        final_parse_stack.shift(token, &self.archive())?;
        if let Err(err) = self.commit_close_family_transaction(CloseFamilyTransaction {
            mem: &prepared.mem,
            memory_body: &prepared.memory_body,
            archive_writes: &prepared.archive_writes,
            events,
            marker_kind: plan.marker_kind,
            close_event: &prepared.close_event,
            event_count,
        }) {
            match err {
                CloseFamilyTransactionError::PreparedSideEffect(err) => {
                    self.parser
                        .replace_parse_stack_for_runtime_transition(pending_close_parse_stack);
                    return Err(err);
                }
                CloseFamilyTransactionError::CommitProof(err) => return Err(err),
            }
        }
        Ok(SpinePreparedCommit {
            kind: plan.kind,
            publication_plan: Some(HistoryPublicationPlan {
                operation: plan.operation,
                suffix_start: prepared.suffix_start,
                replacement_prefix: prepared.replacement,
                preserve_host_history_from: toolcall_start,
                append_current_tool_response_if_missing: true,
            }),
            final_parse_stack: Some(final_parse_stack),
            completed_toolcall: Some(completed_toolcall),
            toolcall_seq: Some(toolcall_seq),
            raw_items: raw_items.to_vec(),
            mem_for_accounting: Some(prepared.mem),
        })
    }

    fn close_family_plan(
        &self,
        prepared: &PreparedCloseCommit,
        after_close: CloseFamilyAfterClose,
    ) -> Result<CloseFamilyPlan, SpineError> {
        match after_close {
            CloseFamilyAfterClose::None => Ok(CloseFamilyPlan {
                operation: "spine.close",
                missing_toolcall_error: "spine.close commit requires completed toolcall evidence",
                event_count_underflow_error: "spine close event count underflow",
                toolcall_seq_overflow_error: "spine.close toolcall seq overflow",
                marker_kind: SpineCommitKindMarker::Close,
                kind: SpineCommitKind::Close,
                toolcall_context_index: None,
                open: None,
            }),
            CloseFamilyAfterClose::Open { summary } => {
                let mut close_reduced_parse_stack = self.parser.parse_stack().clone();
                close_reduced_parse_stack
                    .shift_pending_close(prepared.memory.clone(), &self.archive())?;
                close_reduced_parse_stack
                    .apply_prevalidated_task_tree_reduction(prepared.task_tree_reduction.clone());
                let child = close_reduced_parse_stack.next_child_id()?;
                let open_index = prepared
                    .suffix_start
                    .checked_add(prepared.replacement.len())
                    .ok_or_else(|| {
                        SpineError::InvalidEvent(
                            "spine.next synthetic open index overflow".to_string(),
                        )
                    })?;
                let open_index_u64 = u64::try_from(open_index).map_err(|_| {
                    SpineError::InvalidEvent("spine.next synthetic open index overflow".to_string())
                })?;
                let (event, _token) = crate::spine::lexer::lex_open_event_token(
                    &self.archive(),
                    child.clone(),
                    self.raw_len,
                    open_index_u64,
                    summary.clone(),
                    None,
                    None,
                    None,
                )?;
                Ok(CloseFamilyPlan {
                    operation: "spine.next",
                    missing_toolcall_error: "spine.next commit requires completed toolcall evidence",
                    event_count_underflow_error: "spine next event count underflow",
                    toolcall_seq_overflow_error: "spine.next toolcall seq overflow",
                    marker_kind: SpineCommitKindMarker::CloseThenOpen,
                    kind: SpineCommitKind::CloseThenOpen { open_index },
                    toolcall_context_index: Some(open_index),
                    open: Some(CloseFamilyOpenPlan {
                        child,
                        open_index_u64,
                        summary,
                        event,
                    }),
                })
            }
        }
    }

    fn commit_close_family_transaction(
        &mut self,
        tx: CloseFamilyTransaction<'_>,
    ) -> Result<(), CloseFamilyTransactionError> {
        self.write_prepared_memory_body(tx.mem, tx.memory_body)
            .and_then(|()| flush_archive_writes(tx.archive_writes))
            .and_then(|()| self.commit_prepared_memory_record(tx.mem, tx.memory_body))
            .map_err(CloseFamilyTransactionError::PreparedSideEffect)?;
        let marker = close_commit_marker(
            self.ledger.next_event_seq,
            tx.mem,
            tx.marker_kind,
            close_event_boundary(tx.close_event)
                .map_err(CloseFamilyTransactionError::CommitProof)?,
            tx.event_count,
        )
        .map_err(CloseFamilyTransactionError::CommitProof)?;
        self.append_committed_events(tx.events, marker)
            .map_err(CloseFamilyTransactionError::CommitProof)?;
        Ok(())
    }

    pub(crate) fn persist_prepared_commit_side_effects(
        &mut self,
        prepared: &SpinePreparedCommit,
    ) -> Result<(), SpineError> {
        if let (Some(completed_toolcall), Some(toolcall_seq)) =
            (prepared.completed_toolcall.as_ref(), prepared.toolcall_seq)
        {
            self.append_trim_candidates_for_completed_toolcall(
                completed_toolcall,
                toolcall_seq,
                &prepared.raw_items,
            )?;
        }
        if let Some(mem) = prepared.mem_for_accounting.as_ref() {
            self.register_pending_memory_context_accounting(mem)?;
        }
        Ok(())
    }

    pub(crate) fn persist_prepared_commit_application_side_effects(
        &mut self,
        application: &SpinePreparedCommitApplication,
    ) -> Result<(), SpineError> {
        self.persist_prepared_commit_side_effects(application.as_prepared_commit())
    }

    pub(crate) fn persist_commit_publication_side_effects<T>(
        &mut self,
        publication: &SpineCommitPublication<T>,
    ) -> Result<(), SpineError> {
        if let Some(application) = publication.application() {
            self.persist_prepared_commit_application_side_effects(application)?;
        }
        Ok(())
    }

    pub(crate) fn install_prepared_commit(&mut self, prepared: SpinePreparedCommit) {
        if let Some(final_parse_stack) = prepared.final_parse_stack {
            self.parser
                .replace_parse_stack_for_runtime_transition(final_parse_stack);
        }
        if let Some(completed_toolcall) = prepared.completed_toolcall.as_ref() {
            self.clear_completed_toolcall_anchors(completed_toolcall);
        }
    }

    pub(crate) fn install_prepared_commit_application(
        &mut self,
        application: SpinePreparedCommitApplication,
    ) {
        self.install_prepared_commit(application.into_prepared_commit());
    }

    pub(crate) fn install_commit_publication<T>(
        &mut self,
        publication: SpineCommitPublication<T>,
    ) -> bool {
        if let Some(application) = publication.into_application() {
            self.install_prepared_commit_application(application);
            true
        } else {
            false
        }
    }

    pub(crate) fn commit_publication_history_update<T>(
        &self,
        call_id: &str,
        prepared_commit: Option<&SpinePreparedCommit>,
        tool_resp_item: &ResponseItem,
        tool_resp_already_recorded: bool,
        raw_items: &[Option<ResponseItem>],
        history_items: &[ResponseItem],
        build_update: impl FnOnce(&str, &'static str, usize, Vec<ResponseItem>, Vec<ResponseItem>) -> T,
    ) -> Result<Option<T>, SpineError> {
        let Some((operation, suffix_start, expected_history, replacement)) = self
            .commit_publication_history_update_parts(
                call_id,
                prepared_commit,
                tool_resp_item,
                tool_resp_already_recorded,
                raw_items,
                history_items,
            )?
        else {
            return Ok(None);
        };
        Ok(Some(build_update(
            call_id,
            operation,
            suffix_start,
            expected_history,
            replacement,
        )))
    }

    pub(crate) fn commit_application_publication_history_update<T>(
        &self,
        call_id: &str,
        application: Option<&SpinePreparedCommitApplication>,
        tool_resp_item: &ResponseItem,
        tool_resp_already_recorded: bool,
        raw_items: &[Option<ResponseItem>],
        history_items: &[ResponseItem],
        build_update: impl FnOnce(&str, &'static str, usize, Vec<ResponseItem>, Vec<ResponseItem>) -> T,
    ) -> Result<Option<T>, SpineError> {
        self.commit_publication_history_update(
            call_id,
            application.map(SpinePreparedCommitApplication::as_prepared_commit),
            tool_resp_item,
            tool_resp_already_recorded,
            raw_items,
            history_items,
            build_update,
        )
    }

    pub(crate) fn prepare_commit_publication<T>(
        &self,
        call_id: &str,
        application: Option<SpinePreparedCommitApplication>,
        tool_resp_item: &ResponseItem,
        tool_resp_already_recorded: bool,
        raw_items: &[Option<ResponseItem>],
        history_items: &[ResponseItem],
        build_update: impl FnOnce(&str, &'static str, usize, Vec<ResponseItem>, Vec<ResponseItem>) -> T,
    ) -> Result<SpineCommitPublication<T>, SpineError> {
        if let Some(application) = application.as_ref() {
            application.validate_against_host_history(call_id, history_items)?;
        }
        let history_update = self.commit_application_publication_history_update(
            call_id,
            application.as_ref(),
            tool_resp_item,
            tool_resp_already_recorded,
            raw_items,
            history_items,
            build_update,
        )?;
        Ok(SpineCommitPublication::new(application, history_update))
    }

    fn commit_publication_history_update_parts(
        &self,
        call_id: &str,
        prepared_commit: Option<&SpinePreparedCommit>,
        tool_resp_item: &ResponseItem,
        tool_resp_already_recorded: bool,
        raw_items: &[Option<ResponseItem>],
        history_items: &[ResponseItem],
    ) -> Result<Option<(&'static str, usize, Vec<ResponseItem>, Vec<ResponseItem>)>, SpineError>
    {
        if let Some(plan) = prepared_commit.and_then(|prepared| prepared.publication_plan.as_ref())
        {
            let suffix_end = history_items.len();
            if plan.suffix_start > suffix_end {
                return Err(SpineError::Invariant(format!(
                    "{} suffix start {} exceeds history length {suffix_end} for call_id={call_id}",
                    plan.operation, plan.suffix_start
                )));
            }
            if plan.preserve_host_history_from > suffix_end {
                return Err(SpineError::Invariant(format!(
                    "{} preserve-host-history index {} exceeds history length {suffix_end} for call_id={call_id}",
                    plan.operation, plan.preserve_host_history_from
                )));
            }
            let mut replacement = plan.replacement_prefix.clone();
            replacement.extend_from_slice(&history_items[plan.preserve_host_history_from..]);
            if plan.append_current_tool_response_if_missing && !tool_resp_already_recorded {
                replacement.push(tool_resp_item.clone());
            }
            return Ok(Some((
                plan.operation,
                plan.suffix_start,
                history_items.to_vec(),
                replacement,
            )));
        }
        if !tool_resp_already_recorded {
            return Ok(None);
        }
        let materialized = self.materialize_history(raw_items)?;
        if materialized.as_slice() == history_items {
            return Ok(None);
        }
        Ok(Some((
            "spine toolcall projection",
            0,
            history_items.to_vec(),
            materialized,
        )))
    }

    fn prepare_close_commit(
        &self,
        memory_assembly: Option<SpineCloseMemoryAssembly>,
        token_baselines: SpineTokenBaselines,
    ) -> Result<PreparedCloseCommit, SpineError> {
        let memory_assembly = memory_assembly.ok_or_else(|| {
            SpineError::CompactFailure(
                "spine.close requires a validated source plan for memory assembly".to_string(),
            )
        })?;
        let open_meta = self.current_close_open_meta()?.clone();
        let node = open_meta.id.clone();
        if !self.parser.current_open_has_nodes()? {
            return Err(SpineError::Operation(format!(
                "spine.close requires non-empty live suffix for node {node}"
            )));
        }
        let suffix_start = open_meta.index;
        let seq = self.ledger.next_event_seq;
        if memory_assembly.source_context_range.start != suffix_start {
            return Err(SpineError::CompactFailure(format!(
                "spine.close memory source context range starts at {}, expected suffix start {suffix_start} for node {}",
                memory_assembly.source_context_range.start, open_meta.id
            )));
        }
        let expected_raw_start = self.open_raw_start(&open_meta.id)?;
        if memory_assembly.source_raw_range.start != expected_raw_start {
            return Err(SpineError::CompactFailure(format!(
                "spine.close memory source raw range starts at {}, expected raw start {expected_raw_start} for node {}",
                memory_assembly.source_raw_range.start, open_meta.id
            )));
        }
        if memory_assembly.source_raw_range.end > self.raw_len {
            return Err(SpineError::CompactFailure(format!(
                "spine.close memory source raw range end {} exceeds raw_len {} for node {}",
                memory_assembly.source_raw_range.end, self.raw_len, open_meta.id
            )));
        }
        let body = memory_assembly.body.clone();
        let mem = self.stage_close_mem(&open_meta, &memory_assembly, token_baselines)?;
        let memory = memory_ref(
            &self.archive(),
            mem.compact_id.clone(),
            mem.node.clone(),
            mem.body_hash.clone(),
            mem.raw_start..mem.raw_end,
            mem.context_start..mem.context_end,
            seq..seq + 1,
            mem.open_input_tokens,
            mem.close_input_tokens,
            mem.open_context_tokens,
            mem.close_context_tokens,
            mem.closed_source_suffix_tokens,
            mem.closed_memory_context_tokens,
            mem.open_context_source,
            mem.memory_output_tokens,
        );
        let (close_event, _token) = crate::spine::lexer::lex_close_event_token(
            node,
            self.raw_len,
            open_meta.summary.clone(),
            token_baselines.provider_input_tokens,
            token_baselines.provider_input_tokens,
            memory.clone(),
        )?;
        let staged_archive = SpineArchive::staged_with_memory_body(
            self.store.root.clone(),
            mem.compact_id.clone(),
            body.clone(),
        );
        let task_tree_reduction = self
            .parser
            .parse_stack()
            .prepare_current_task_tree_reduction(&staged_archive, memory.clone())?;
        let archive_writes = staged_archive.staged_writes();
        let replacement = vec![memory_response_item(&body)];
        Ok(PreparedCloseCommit {
            suffix_start,
            replacement,
            mem,
            memory_body: body,
            archive_writes,
            close_event,
            memory,
            task_tree_reduction,
        })
    }

    #[cfg(test)]
    pub(in crate::spine) fn prepared_close_memory_for_test(
        &self,
        memory_assembly: Option<SpineCloseMemoryAssembly>,
        token_baselines: SpineTokenBaselines,
    ) -> Result<crate::spine::model::MemoryRef, SpineError> {
        self.prepare_close_commit(memory_assembly, token_baselines)
            .map(|prepared| prepared.memory)
    }

    #[cfg(test)]
    fn observed_completed_toolcall(
        &self,
        call_id: &str,
    ) -> Result<Option<CompletedToolCall>, SpineError> {
        let Some(responses) = self.pending_tool_responses.get(call_id) else {
            return Ok(None);
        };
        if responses.is_empty() {
            return Ok(None);
        }
        let request = self.pending_tool_request_anchor(call_id)?;
        let mut response_context_indices = Vec::with_capacity(responses.len());
        for response in responses {
            response_context_indices.push(usize::try_from(response.context_index).map_err(
                |_| SpineError::InvalidEvent("tool response context index overflow".to_string()),
            )?);
        }
        Ok(Some(CompletedToolCall {
            call_id: call_id.to_string(),
            request_call_ids: vec![call_id.to_string()],
            segments: std::iter::once(CompletedToolCallSegment {
                kind: ToolCallSegmentKind::Request,
                raw_ordinal: request.raw_ordinal,
                context_index: request.context_index,
            })
            .chain(responses.iter().zip(response_context_indices).map(
                |(response, context_index)| CompletedToolCallSegment {
                    kind: ToolCallSegmentKind::Response,
                    raw_ordinal: response.raw_ordinal,
                    context_index,
                },
            ))
            .collect(),
        }))
    }
}
