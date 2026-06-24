use codex_protocol::models::ResponseItem;

use super::SpineError;
use super::SpineRuntime;
use super::close_family::CloseFamilyAfterClose;
use super::close_family::CloseFamilyPlan;
use super::close_family::CloseFamilyTransaction;
use super::close_family::CloseFamilyTransactionError;
use super::close_family::PreparedCloseCommit;
use super::pending::CompletedToolCall;
#[cfg(test)]
use super::pending::CompletedToolCallSegment;
use super::pending::PendingTransition;
use super::prepared::SpineCommitKind;
use super::prepared::SpineCommitPublication;
use super::prepared::SpinePreparedCommit;
use super::prepared::SpinePreparedCommitInstall;
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
#[cfg(test)]
use crate::spine::model::ToolCallSegmentKind;
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
    ) -> Result<Option<SpinePreparedCommitInstall>, SpineError> {
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
        .map(|prepared| prepared.map(SpinePreparedCommit::into_install))
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
        let open_lexed = crate::spine::lexer::lex_open(
            &self.archive(),
            child,
            boundary,
            index,
            summary,
            token_baselines.provider_input_tokens,
            token_baselines.provider_input_tokens,
            open_context_source,
        )?;
        if let Some(completed_toolcall) = completed_toolcall {
            let toolcall_lexed = self.completed_toolcall_batch(&completed_toolcall)?;
            let parser_install = self.parser.prepare_open_install(
                &open_lexed,
                Some(&toolcall_lexed),
                &self.archive(),
            )?;
            let toolcall_seq = self.ledger.next_event_seq.checked_add(1).ok_or_else(|| {
                SpineError::InvalidEvent("spine.open toolcall seq overflow".to_string())
            })?;
            let mut events = open_lexed.into_events();
            events.extend(toolcall_lexed.into_events());
            self.append_committed_events_no_marker(events)?;
            return Ok(SpinePreparedCommit::open_with_toolcall(
                SpineCommitKind::Open {
                    open_request_index: usize::try_from(index).map_err(|_| {
                        SpineError::InvalidEvent("spine.open context index overflow".to_string())
                    })?,
                },
                parser_install.into_commit_install(),
                completed_toolcall,
                toolcall_seq,
                raw_items.to_vec(),
            ));
        }
        let parser_install =
            self.parser
                .prepare_open_install(&open_lexed, None, &self.archive())?;
        let events = open_lexed.into_events();
        self.append_committed_events_no_marker(events)?;
        self.parser.install_prepared_open(parser_install);
        Ok(SpinePreparedCommit::installed_open(SpineCommitKind::Open {
            open_request_index: usize::try_from(index).map_err(|_| {
                SpineError::InvalidEvent("spine.open context index overflow".to_string())
            })?,
        }))
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
        plan.append_open_events(&mut events);
        let completed_toolcall = plan.require_completed_toolcall(completed_toolcall)?;
        let toolcall_start = completed_toolcall_first_segment(&completed_toolcall)?.context_index;
        let toolcall_context_index = plan.toolcall_context_index(&prepared)?;
        let completed_toolcall = self
            .remap_completed_toolcall_context_indices(completed_toolcall, toolcall_context_index)?;
        let toolcall_lexed = self.completed_toolcall_batch(&completed_toolcall)?;
        events.extend(toolcall_lexed.events().iter().cloned());
        let event_count = plan.event_count(events.len())?;
        let toolcall_seq = plan.toolcall_seq(self.ledger.next_event_seq, event_count)?;
        let (pending_parser_install, parser_install) =
            self.parser.close_family_staged_parse_stacks(
                prepared.memory.clone(),
                prepared.task_tree_reduction,
                plan.open_lexed(),
                &toolcall_lexed,
                &self.archive(),
            )?;
        if let Err(err) = self.commit_close_family_transaction(CloseFamilyTransaction {
            mem: &prepared.mem,
            memory_body: &prepared.memory_body,
            archive_writes: &prepared.archive_writes,
            events,
            marker_kind: plan.marker_kind(),
            close_event: &prepared.close_event,
            event_count,
        }) {
            match err {
                CloseFamilyTransactionError::PreparedSideEffect(err) => {
                    self.parser
                        .install_pending_close_after_side_effect_failure(pending_parser_install);
                    return Err(err);
                }
                CloseFamilyTransactionError::CommitProof(err) => return Err(err),
            }
        }
        Ok(SpinePreparedCommit::close_family(
            plan.kind(),
            self.parser.close_family_publication_plan(
                plan.operation(),
                prepared.suffix_start,
                prepared.replacement,
                toolcall_start,
            ),
            parser_install,
            completed_toolcall,
            toolcall_seq,
            raw_items.to_vec(),
            prepared.mem,
        ))
    }

    fn close_family_plan(
        &self,
        prepared: &PreparedCloseCommit,
        after_close: CloseFamilyAfterClose,
    ) -> Result<CloseFamilyPlan, SpineError> {
        match after_close {
            CloseFamilyAfterClose::None => Ok(CloseFamilyPlan::close()),
            CloseFamilyAfterClose::Open { summary } => {
                let child = self.parser.close_reduced_next_child_id(
                    prepared.memory.clone(),
                    prepared.task_tree_reduction.clone(),
                    &self.archive(),
                )?;
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
                let lexed = crate::spine::lexer::lex_open(
                    &self.archive(),
                    child.clone(),
                    self.raw_len,
                    open_index_u64,
                    summary.clone(),
                    None,
                    None,
                    None,
                )?;
                Ok(CloseFamilyPlan::next(open_index, lexed))
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

    pub(crate) fn persist_commit_publication_side_effects<T>(
        &mut self,
        publication: &SpineCommitPublication<T>,
    ) -> Result<(), SpineError> {
        if let Some(install) = publication.install() {
            self.persist_prepared_commit_side_effects(install.as_prepared_commit())?;
        }
        Ok(())
    }

    pub(crate) fn install_prepared_commit(&mut self, prepared: SpinePreparedCommit) {
        if let Some(parser_install) = prepared.parser_install {
            self.parser.install_prepared_commit(parser_install);
        }
        if let Some(completed_toolcall) = prepared.completed_toolcall.as_ref() {
            self.clear_completed_toolcall_anchors(completed_toolcall);
        }
    }

    pub(crate) fn install_commit_publication<T>(
        &mut self,
        publication: SpineCommitPublication<T>,
    ) -> bool {
        if let Some(install) = publication.into_install() {
            self.install_prepared_commit(install.into_prepared_commit());
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
        self.parser_commit_publication_history_update(
            call_id,
            prepared_commit,
            tool_resp_item,
            tool_resp_already_recorded,
            raw_items,
            history_items,
            build_update,
        )
    }

    pub(crate) fn commit_install_publication_history_update<T>(
        &self,
        call_id: &str,
        install: Option<&SpinePreparedCommitInstall>,
        tool_resp_item: &ResponseItem,
        tool_resp_already_recorded: bool,
        raw_items: &[Option<ResponseItem>],
        history_items: &[ResponseItem],
        build_update: impl FnOnce(&str, &'static str, usize, Vec<ResponseItem>, Vec<ResponseItem>) -> T,
    ) -> Result<Option<T>, SpineError> {
        self.commit_publication_history_update(
            call_id,
            install.map(SpinePreparedCommitInstall::as_prepared_commit),
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
        install: Option<SpinePreparedCommitInstall>,
        tool_resp_item: &ResponseItem,
        tool_resp_already_recorded: bool,
        raw_items: &[Option<ResponseItem>],
        history_items: &[ResponseItem],
        build_update: impl FnOnce(&str, &'static str, usize, Vec<ResponseItem>, Vec<ResponseItem>) -> T,
    ) -> Result<SpineCommitPublication<T>, SpineError> {
        if let Some(install) = install.as_ref() {
            install.validate_against_host_history(call_id, history_items)?;
        }
        let history_update = self.commit_install_publication_history_update(
            call_id,
            install.as_ref(),
            tool_resp_item,
            tool_resp_already_recorded,
            raw_items,
            history_items,
            build_update,
        )?;
        Ok(SpineCommitPublication::new(install, history_update))
    }

    fn parser_commit_publication_history_update<T>(
        &self,
        call_id: &str,
        prepared_commit: Option<&SpinePreparedCommit>,
        tool_resp_item: &ResponseItem,
        tool_resp_already_recorded: bool,
        raw_items: &[Option<ResponseItem>],
        history_items: &[ResponseItem],
        build_update: impl FnOnce(&str, &'static str, usize, Vec<ResponseItem>, Vec<ResponseItem>) -> T,
    ) -> Result<Option<T>, SpineError> {
        let update =
            if let Some(plan) = prepared_commit.and_then(SpinePreparedCommit::publication_plan) {
                plan.history_update(
                    call_id,
                    tool_resp_item,
                    tool_resp_already_recorded,
                    history_items,
                )?
            } else if !tool_resp_already_recorded {
                return Ok(None);
            } else {
                let trim_projection = self.current_trim_projection()?;
                if let Some(parser_install) =
                    prepared_commit.and_then(SpinePreparedCommit::parser_install)
                {
                    parser_install.full_context_publication_update(
                        "spine prepared commit projection",
                        raw_items,
                        &trim_projection,
                        history_items,
                    )?
                } else {
                    self.parser.full_variable_context_publication_update(
                        "spine toolcall projection",
                        raw_items,
                        &trim_projection,
                        history_items,
                    )?
                }
            };
        Ok(update.map(|update| update.into_history_update(call_id, build_update)))
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
        let mut close_events = crate::spine::lexer::lex_close(
            node,
            self.raw_len,
            open_meta.summary.clone(),
            token_baselines.provider_input_tokens,
            token_baselines.provider_input_tokens,
            memory.clone(),
        )?
        .into_events()
        .into_iter();
        let close_event = close_events
            .next()
            .ok_or_else(|| SpineError::Invariant("close lexer produced no event".to_string()))?;
        if close_events.next().is_some() {
            return Err(SpineError::Invariant(
                "close lexer produced multiple events".to_string(),
            ));
        }
        let staged_archive = SpineArchive::staged_with_memory_body(
            self.store.root.clone(),
            mem.compact_id.clone(),
            body.clone(),
        );
        let task_tree_reduction = self
            .parser
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
