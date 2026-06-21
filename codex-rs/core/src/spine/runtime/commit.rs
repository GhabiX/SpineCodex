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
#[cfg(test)]
use super::pending::PendingToolResponse;
use super::pending::PendingTransition;
use super::prepared::HistoryPublicationPlan;
use super::prepared::SpineCommitKind;
use super::prepared::SpinePreparedCommit;
use super::support::close_commit_marker;
use super::support::close_event_boundary;
use super::support::completed_toolcall_first_segment;
use super::types::SpineCloseMemoryAssembly;
use super::types::SpineTokenBaselines;
use crate::spine::archive::SpineArchive;
use crate::spine::archive::flush_archive_writes;
use crate::spine::archive::memory_ref;
use crate::spine::archive::tree_meta_with_token_baselines;
use crate::spine::model::ContextBaselineSource;
use crate::spine::model::SpineCommitKindMarker;
use crate::spine::model::SpineLedgerEvent;
use crate::spine::model::SpineToken;
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

    fn maybe_commit_output_impl(
        &mut self,
        call_id: &str,
        memory_assembly: Option<SpineCloseMemoryAssembly>,
        token_baselines: SpineTokenBaselines,
        completed_toolcall: Option<CompletedToolCall>,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<Option<SpinePreparedCommit>, SpineError> {
        self.ensure_pending_from_receipt(call_id)?;
        let Some(pending) = self.pending.as_ref() else {
            return Ok(None);
        };
        if pending.call_id() != call_id {
            return Ok(None);
        }
        let pending = pending.clone();
        let commit_kind = match pending {
            PendingTransition::Open {
                summary,
                boundary,
                index,
                ..
            } => self.commit_open_pending(
                summary,
                boundary,
                index,
                token_baselines,
                completed_toolcall,
                raw_items,
            )?,
            PendingTransition::Close { .. } => self.commit_close_pending(
                memory_assembly,
                token_baselines,
                completed_toolcall,
                raw_items,
            )?,
            PendingTransition::NextSugar { summary, .. } => self.commit_next_sugar_pending(
                summary,
                memory_assembly,
                token_baselines,
                completed_toolcall,
                raw_items,
            )?,
        };
        self.pending = None;
        self.control_call_ids.remove(call_id);
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
        let child = self.parse_stack.next_child_id()?;
        let open_context_source = token_baselines
            .provider_input_tokens
            .map(|_| ContextBaselineSource::ProviderAtOpen);
        let event = SpineLedgerEvent::Open {
            child: child.clone(),
            boundary,
            index,
            summary: summary.clone(),
            open_input_tokens: token_baselines.provider_input_tokens,
            open_context_tokens: token_baselines.provider_input_tokens,
            open_context_source,
        };
        let mut staged_parse_stack = self.parse_stack.clone();
        staged_parse_stack.shift(
            SpineToken::Open {
                meta: tree_meta_with_token_baselines(
                    &self.archive(),
                    child,
                    index,
                    summary,
                    token_baselines.provider_input_tokens,
                    open_context_source,
                )?,
            },
            &self.archive(),
        )?;
        if let Some(completed_toolcall) = completed_toolcall {
            let (toolcall_event, segments) = self.completed_toolcall_parts(&completed_toolcall)?;
            staged_parse_stack.shift(SpineToken::ToolCall { segments }, &self.archive())?;
            let toolcall_seq = self.ledger.next_event_seq.checked_add(1).ok_or_else(|| {
                SpineError::InvalidEvent("spine.open toolcall seq overflow".to_string())
            })?;
            let events = vec![event, toolcall_event];
            self.append_committed_events_no_marker(events)?;
            self.parse_stack = staged_parse_stack;
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
        let events = vec![event];
        self.append_committed_events_no_marker(events)?;
        self.parse_stack = staged_parse_stack;
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
        let (toolcall_event, segments) = self.completed_toolcall_parts(&completed_toolcall)?;
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
        let mut pending_close_parse_stack = self.parse_stack.clone();
        pending_close_parse_stack.shift_pending_close(prepared.memory.clone(), &self.archive())?;
        let mut final_parse_stack = self.task_tree_reduced_from(
            pending_close_parse_stack.clone(),
            prepared.task_tree_reduction,
        )?;
        if let Some(open) = plan.open.as_ref() {
            final_parse_stack.shift(
                SpineToken::Open {
                    meta: tree_meta_with_token_baselines(
                        &self.archive(),
                        open.child.clone(),
                        open.open_index_u64,
                        open.summary.clone(),
                        None,
                        None,
                    )?,
                },
                &self.archive(),
            )?;
        }
        final_parse_stack.shift(SpineToken::ToolCall { segments }, &self.archive())?;
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
                    self.parse_stack = pending_close_parse_stack;
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
                let mut close_reduced_parse_stack = self.parse_stack.clone();
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
                let event = SpineLedgerEvent::Open {
                    child: child.clone(),
                    boundary: self.raw_len,
                    index: open_index_u64,
                    summary: summary.clone(),
                    open_input_tokens: None,
                    open_context_tokens: None,
                    open_context_source: None,
                };
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

    pub(crate) fn install_prepared_commit(&mut self, prepared: SpinePreparedCommit) {
        if let Some(final_parse_stack) = prepared.final_parse_stack {
            self.parse_stack = final_parse_stack;
        }
        if let Some(completed_toolcall) = prepared.completed_toolcall.as_ref() {
            self.clear_completed_toolcall_anchors(completed_toolcall);
        }
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
        if !self.parse_stack.current_open_has_nodes()? {
            return Err(SpineError::Operation(format!(
                "spine.close requires non-empty live suffix for node {node}"
            )));
        }
        let suffix_start = open_meta.index;
        let close_event = SpineLedgerEvent::Close {
            node,
            boundary: self.raw_len,
            summary: open_meta.summary.clone(),
            close_input_tokens: token_baselines.provider_input_tokens,
            close_context_tokens: token_baselines.provider_input_tokens,
        };
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
        let staged_archive = SpineArchive::staged_with_memory_body(
            self.store.root.clone(),
            mem.compact_id.clone(),
            body.clone(),
        );
        let task_tree_reduction = self
            .parse_stack
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
