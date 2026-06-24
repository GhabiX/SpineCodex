#[cfg(test)]
use codex_protocol::spine_tree::SpineNodeContextBaselineSource;
use codex_protocol::spine_tree::SpineNodeContextProblem;
use codex_protocol::spine_tree::SpineTreeUpdateEvent;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

use super::OpenContextBaseline;
use super::PendingMemoryContextAccounting;
use super::SpineError;
use super::SpineOpenNodeContextProjection;
use super::SpineRuntime;
use super::replay::live_context_baseline_source;
use super::replay::protocol_context_baseline_source;
use crate::spine::io::hash_raw_live;
use crate::spine::model::ContextBaselineSource;
use crate::spine::model::MemRecord;
use crate::spine::model::MemoryContextAccountingRecord;
use crate::spine::model::MemoryContextAccountingSkipReason;
use crate::spine::model::MemoryContextAccountingWitnessRecord;
use crate::spine::model::NodeId;
use crate::spine::model::SpineCommitKindMarker;
use crate::spine::model::SpineLedgerEvent;
use crate::spine::model::TreeMeta;
use crate::spine::store::SpineStore;

impl SpineRuntime {
    #[cfg(test)]
    pub(crate) fn render_tree(&self) -> Result<String, SpineError> {
        self.parse_stack_with_memory_context_accounting()?
            .render_tree()
    }

    pub(crate) fn render_tree_with_context_annotations(
        &self,
        annotations: &BTreeMap<NodeId, String>,
    ) -> Result<String, SpineError> {
        self.ensure_jit_enabled("Spine tree render")?;
        self.parse_stack_with_memory_context_accounting()?
            .render_tree_with_context_annotations(annotations)
    }

    pub(crate) fn build_tree_snapshot(&self) -> Result<SpineTreeUpdateEvent, SpineError> {
        self.ensure_jit_enabled("Spine tree snapshot")?;
        let parse_stack = self.parse_stack_with_memory_context_accounting()?;
        let nodes = parse_stack.tree_snapshot_nodes()?;
        let active_node_id = parse_stack.current_cursor_id()?.as_path();
        Ok(SpineTreeUpdateEvent {
            snapshot_seq: self.ledger.next_event_seq,
            active_node_id,
            nodes,
        })
    }

    pub(super) fn parse_stack_with_memory_context_accounting(
        &self,
    ) -> Result<crate::spine::parse_stack::ParseStack, SpineError> {
        let accounting = self.memory_context_accounting_by_id()?;
        Ok(self
            .parser
            .parse_stack_with_memory_context_accounting(&accounting))
    }

    pub(super) fn memory_context_accounting_by_id(
        &self,
    ) -> Result<BTreeMap<String, i64>, SpineError> {
        let mut out = BTreeMap::new();
        for record in self.store.mem_accounting()? {
            match out.get(&record.compact_id).copied() {
                Some(existing) if existing != record.closed_memory_context_tokens => {
                    return Err(SpineError::InvalidStore(format!(
                        "conflicting Spine memory context accounting for {}",
                        record.compact_id
                    )));
                }
                Some(_) => {}
                None => {
                    out.insert(record.compact_id, record.closed_memory_context_tokens);
                }
            }
        }
        Ok(out)
    }

    #[cfg(test)]
    pub(crate) fn current_open_provider_input_tokens(&self) -> Option<i64> {
        self.current_open_context_baseline()
            .map(|baseline| baseline.provider_input_tokens)
    }

    #[cfg(test)]
    pub(crate) fn current_open_context_baseline_source(
        &self,
    ) -> Option<SpineNodeContextBaselineSource> {
        self.current_open_context_baseline()
            .map(|baseline| baseline.source)
            .map(protocol_context_baseline_source)
    }

    #[cfg(test)]
    pub(super) fn current_open_context_baseline(&self) -> Option<OpenContextBaseline> {
        self.parser
            .current_open_meta_cloned()
            .as_ref()
            .and_then(|meta| self.open_context_baseline_for(meta).ok().flatten())
    }

    pub(crate) fn open_node_context_projections(&self) -> Vec<SpineOpenNodeContextProjection> {
        if !self.jit_enabled {
            return Vec::new();
        }
        self.parser
            .live_open_metas_cloned()
            .into_iter()
            .map(|meta| {
                let (baseline, problem) = match self.open_context_baseline_for(&meta) {
                    Ok(baseline) => (baseline, None),
                    Err(problem) => (None, Some(problem)),
                };
                SpineOpenNodeContextProjection {
                    node_id: meta.id.clone(),
                    provider_input_tokens: baseline.map(|baseline| baseline.provider_input_tokens),
                    baseline_source: baseline
                        .map(|baseline| baseline.source)
                        .map(protocol_context_baseline_source),
                    problem,
                }
            })
            .collect()
    }

    pub(super) fn open_context_baseline_for(
        &self,
        meta: &TreeMeta,
    ) -> Result<Option<OpenContextBaseline>, SpineNodeContextProblem> {
        let source = match live_context_baseline_source(
            meta.open_context_source
                .unwrap_or(ContextBaselineSource::ProviderAtOpen),
        ) {
            Some(source) => source,
            None => return Ok(None),
        };
        match (meta.open_input_tokens, meta.open_context_tokens) {
            (Some(provider_input_tokens), Some(open_context_tokens))
                if provider_input_tokens == open_context_tokens =>
            {
                Ok(Some(OpenContextBaseline {
                    provider_input_tokens,
                    source,
                }))
            }
            (None, None) => Ok(None),
            (Some(_), Some(_)) | (Some(_), None) | (None, Some(_)) => {
                Err(SpineNodeContextProblem::CorruptPressureMetadata)
            }
        }
    }

    pub(crate) fn capture_current_open_provider_baseline(
        &mut self,
        input_tokens: i64,
    ) -> Result<bool, SpineError> {
        if !self.jit_enabled || input_tokens <= 0 {
            return Ok(false);
        }
        let open_meta = match self.parser.current_open_meta_cloned() {
            Some(meta) => meta,
            None => return Ok(false),
        };
        if open_meta.open_context_tokens.is_some() {
            return Ok(false);
        }
        if !self.current_open_accepts_deferred_provider_baseline(&open_meta)? {
            return Ok(false);
        }
        let event = SpineLedgerEvent::OpenContextBaseline {
            node: open_meta.id.clone(),
            raw_boundary: self.raw_len,
            raw_live_hash: hash_raw_live(&self.raw_live),
            open_input_tokens: input_tokens,
            open_context_tokens: input_tokens,
            open_context_source: ContextBaselineSource::ProviderAtOpen,
        };
        self.append_cached_event(event)?;
        self.parser.set_live_open_context_baseline(
            &open_meta.id,
            input_tokens,
            ContextBaselineSource::ProviderAtOpen,
        )
    }

    pub(crate) fn capture_closed_memory_context_accounting(
        &mut self,
        provider_input_tokens: i64,
    ) -> Result<bool, SpineError> {
        if !self.jit_enabled {
            return Ok(false);
        }
        let Some(pending) = self.pending_memory_context_accounting.take() else {
            return Ok(false);
        };
        if provider_input_tokens <= 0 {
            return self.consume_memory_context_accounting_pending_and_skip(
                pending,
                None,
                MemoryContextAccountingSkipReason::MissingProviderUsage,
            );
        }
        if self
            .memory_context_accounting_by_id()?
            .contains_key(&pending.compact_id)
        {
            return self.consume_memory_context_accounting_pending_and_skip(
                pending,
                Some(provider_input_tokens),
                MemoryContextAccountingSkipReason::InvalidProviderUsage,
            );
        }
        if let Some(close_input_tokens) = pending.close_input_tokens
            && provider_input_tokens >= close_input_tokens
        {
            return self.consume_memory_context_accounting_pending_and_skip(
                pending,
                Some(provider_input_tokens),
                MemoryContextAccountingSkipReason::InvalidProviderUsage,
            );
        }
        let memory_tokens = provider_input_tokens - pending.replacement_prefix_baseline_tokens;
        if memory_tokens < 0 {
            return self.consume_memory_context_accounting_pending_and_skip(
                pending,
                Some(provider_input_tokens),
                MemoryContextAccountingSkipReason::NegativeMemoryDelta,
            );
        }
        self.store
            .append_mem_accounting(&MemoryContextAccountingRecord {
                compact_id: pending.compact_id.clone(),
                closed_memory_context_tokens: memory_tokens,
                provider_input_tokens,
                replacement_prefix_baseline_tokens: pending.replacement_prefix_baseline_tokens,
            })?;
        Ok(true)
    }

    pub(crate) fn consume_closed_memory_context_accounting_without_provider_usage(
        &mut self,
    ) -> Result<bool, SpineError> {
        if !self.jit_enabled {
            return Ok(false);
        }
        let Some(pending) = self.pending_memory_context_accounting.take() else {
            return Ok(false);
        };
        self.consume_memory_context_accounting_pending(
            pending,
            None,
            MemoryContextAccountingSkipReason::MissingProviderUsage,
        )?;
        Ok(true)
    }

    fn consume_memory_context_accounting_pending(
        &self,
        pending: PendingMemoryContextAccounting,
        provider_input_tokens: Option<i64>,
        reason: MemoryContextAccountingSkipReason,
    ) -> Result<(), SpineError> {
        self.store
            .append_mem_accounting_witness(&MemoryContextAccountingWitnessRecord::Consumed {
                compact_id: pending.compact_id,
                provider_input_tokens,
                reason,
            })
    }

    fn consume_memory_context_accounting_pending_and_skip(
        &self,
        pending: PendingMemoryContextAccounting,
        provider_input_tokens: Option<i64>,
        reason: MemoryContextAccountingSkipReason,
    ) -> Result<bool, SpineError> {
        self.consume_memory_context_accounting_pending(pending, provider_input_tokens, reason)?;
        Ok(false)
    }

    fn append_memory_context_accounting_pending(
        &mut self,
        pending: PendingMemoryContextAccounting,
    ) -> Result<(), SpineError> {
        self.consume_superseded_memory_context_accounting_pending()?;
        if self
            .memory_context_accounting_by_id()?
            .contains_key(&pending.compact_id)
        {
            return Ok(());
        }
        self.store.append_mem_accounting_witness(
            &MemoryContextAccountingWitnessRecord::Pending {
                compact_id: pending.compact_id.clone(),
                replacement_prefix_baseline_tokens: pending.replacement_prefix_baseline_tokens,
                close_input_tokens: pending.close_input_tokens,
            },
        )?;
        self.pending_memory_context_accounting = Some(pending);
        Ok(())
    }

    fn consume_superseded_memory_context_accounting_pending(&mut self) -> Result<(), SpineError> {
        if let Some(existing) = self.pending_memory_context_accounting.take() {
            self.consume_memory_context_accounting_pending(
                existing,
                None,
                MemoryContextAccountingSkipReason::SupersededByNewPending,
            )?;
        }
        Ok(())
    }

    fn current_open_accepts_deferred_provider_baseline(
        &self,
        open_meta: &TreeMeta,
    ) -> Result<bool, SpineError> {
        if open_meta.summary == "root" && open_meta.id.is_root_epoch_child() {
            return Ok(true);
        }
        let Some(open_seq) = self
            .ledger
            .events
            .iter()
            .rev()
            .find_map(|event| match &event.event {
                SpineLedgerEvent::Open { child, .. } if child == &open_meta.id => Some(event.seq),
                _ => None,
            })
        else {
            return Ok(false);
        };
        Ok(self.store.commit_markers()?.iter().any(|marker| {
            marker.kind == SpineCommitKindMarker::CloseThenOpen
                && marker
                    .token_seq_start
                    .checked_add(1)
                    .is_some_and(|seq| seq == open_seq)
        }))
    }

    pub(super) fn register_pending_memory_context_accounting(
        &mut self,
        mem: &MemRecord,
    ) -> Result<(), SpineError> {
        let Some(baseline) = replacement_prefix_baseline_tokens(mem) else {
            self.consume_superseded_memory_context_accounting_pending()?;
            return Ok(());
        };
        self.append_memory_context_accounting_pending(PendingMemoryContextAccounting {
            compact_id: mem.compact_id.clone(),
            replacement_prefix_baseline_tokens: baseline,
            close_input_tokens: mem.close_input_tokens,
        })
    }
}

fn replacement_prefix_baseline_tokens(mem: &MemRecord) -> Option<i64> {
    if mem.context_start == 0 {
        return Some(0);
    }
    mem.open_context_tokens
}

pub(super) fn pending_memory_context_accounting_from_store(
    store: &SpineStore,
) -> Result<Option<PendingMemoryContextAccounting>, SpineError> {
    let accounted = store
        .mem_accounting()?
        .into_iter()
        .map(|record| record.compact_id)
        .collect::<BTreeSet<_>>();
    let mut pending_by_id = BTreeMap::new();
    for witness in store.mem_accounting_witnesses()? {
        match witness {
            MemoryContextAccountingWitnessRecord::Pending {
                compact_id,
                replacement_prefix_baseline_tokens,
                close_input_tokens,
            } => {
                if !accounted.contains(&compact_id) {
                    pending_by_id.insert(
                        compact_id.clone(),
                        PendingMemoryContextAccounting {
                            compact_id,
                            replacement_prefix_baseline_tokens,
                            close_input_tokens,
                        },
                    );
                }
            }
            MemoryContextAccountingWitnessRecord::Consumed { compact_id, .. } => {
                pending_by_id.remove(&compact_id);
            }
        }
    }
    if pending_by_id.len() > 1 {
        return Err(SpineError::InvalidStore(
            "multiple unconsumed Spine memory context accounting pending witnesses".to_string(),
        ));
    }
    Ok(pending_by_id.into_values().next())
}
