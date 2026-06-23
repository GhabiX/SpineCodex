//! Parser boundary for Spine token consumption and variable context projection.
//!
//! The intended ownership chain is:
//!
//! ```text
//! hook -> lexer -> parser -> PS -> h(PS) -> host publication
//! ```
//!
//! `ParserState` is the production owner of the live parse stack. Runtime code
//! may provide evidence and durable side effects, but parser-visible tokens
//! enter through this facade.

use codex_protocol::models::ResponseItem;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;

use crate::spine::SpineError;
use crate::spine::archive::SpineArchive;
use crate::spine::checkpoint::SpineCheckpoint;
use crate::spine::checkpoint::build_checkpoint;
use crate::spine::lexer::LexedTokenBatch;
use crate::spine::model::ContextBaselineSource;
use crate::spine::model::ControlSymbol;
use crate::spine::model::LoggedSpineLedgerEvent;
use crate::spine::model::MemRecord;
use crate::spine::model::MemoryRef;
use crate::spine::model::NodeId;
use crate::spine::model::RawMask;
use crate::spine::model::SpineToken;
use crate::spine::model::SpineTreeNode;
use crate::spine::model::Symbol;
use crate::spine::model::TreeMeta;
use crate::spine::model::TrimProjection;
use crate::spine::parse_stack::ParseStack;
use crate::spine::parse_stack::PreparedRootEpochReduction;
use crate::spine::parse_stack::PreparedTaskTreeReduction;
use crate::spine::parse_stack::apply_metadata_event;
use crate::spine::parse_stack::event_to_token;
#[cfg(test)]
use crate::spine::parse_stack::parse_stack_msg_leaf_count;
#[cfg(test)]
use crate::spine::parse_stack::parse_stack_toolcall_leaf_count;
use crate::spine::render::render_parse_stack_to_context_with_memory_body_and_trim_projection;
use crate::spine::render::render_parse_stack_to_context_with_trim_projection;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ParserState {
    parse_stack: ParseStack,
}

pub(super) struct ParserRootCompactPreparedReduction {
    pub(super) final_parse_stack: ParserPreparedState,
    pub(super) root_epoch_reduction: PreparedRootEpochReduction,
    pub(super) materialized: Vec<ResponseItem>,
    pub(super) current_open_index: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ParserPreparedState {
    parse_stack: ParseStack,
}

impl ParserPreparedState {
    fn new(parse_stack: ParseStack) -> Self {
        Self { parse_stack }
    }

    pub(super) fn parse_stack(&self) -> &ParseStack {
        &self.parse_stack
    }

    fn into_parse_stack(self) -> ParseStack {
        self.parse_stack
    }
}

impl ParserState {
    pub(super) fn new() -> Self {
        Self {
            parse_stack: ParseStack::new(),
        }
    }

    pub(super) fn from_parse_stack(parse_stack: ParseStack) -> Self {
        Self { parse_stack }
    }

    pub(super) fn from_replay_events_with_forced_events(
        events: &[LoggedSpineLedgerEvent],
        archive: &SpineArchive,
        mems: &[MemRecord],
        raw_mask: RawMask<'_>,
        forced_event_seqs: &BTreeSet<u64>,
        marker_structural_event_seqs: &BTreeSet<u64>,
    ) -> Result<Self, SpineError> {
        let mems = mems
            .iter()
            .cloned()
            .map(|mem| (mem.compact_id.clone(), mem))
            .collect::<BTreeMap<_, _>>();
        let mut parser = Self::new();
        for event in events {
            if forced_event_seqs.contains(&event.seq)
                || (!marker_structural_event_seqs.contains(&event.seq)
                    && event.allowed_by(raw_mask)?)
            {
                parser.apply_replay_event(event, archive, &mems, raw_mask)?;
            }
        }
        Ok(parser)
    }

    pub(super) fn parse_stack(&self) -> &ParseStack {
        &self.parse_stack
    }

    pub(super) fn parse_stack_with_memory_context_accounting(
        &self,
        accounting: &BTreeMap<String, i64>,
    ) -> ParseStack {
        let mut parse_stack = self.parse_stack.clone();
        parse_stack.apply_memory_context_accounting(accounting);
        parse_stack
    }

    pub(super) fn current_open_meta_cloned(&self) -> Option<TreeMeta> {
        self.parse_stack.current_open_meta_opt().cloned()
    }

    pub(super) fn live_open_metas_cloned(&self) -> Vec<TreeMeta> {
        self.parse_stack
            .live_open_metas()
            .into_iter()
            .cloned()
            .collect()
    }

    pub(super) fn current_open_suffix_nodes_cloned(
        &self,
    ) -> Result<Vec<SpineTreeNode>, SpineError> {
        let open_idx = self
            .parse_stack
            .symbols
            .iter()
            .rposition(|symbol| matches!(symbol, Symbol::Control(ControlSymbol::Open(_))))
            .ok_or_else(|| SpineError::InvalidEvent("ParseStack has no live Open".to_string()))?;
        let suffix = &self.parse_stack.symbols[open_idx + 1..];
        match suffix {
            [Symbol::SpineTreeNodes(nodes)]
            | [
                Symbol::SpineTreeNodes(nodes),
                Symbol::Control(ControlSymbol::Close(_)),
            ] => Ok(nodes.clone()),
            _ => Err(SpineError::InvalidEvent(format!(
                "spine.close source plan expected live node list after current Open, found {suffix:?}"
            ))),
        }
    }

    pub(super) fn current_open_has_nodes(&self) -> Result<bool, SpineError> {
        self.parse_stack.current_open_has_nodes()
    }

    pub(super) fn prepare_current_task_tree_reduction(
        &self,
        archive: &SpineArchive,
        memory: MemoryRef,
    ) -> Result<PreparedTaskTreeReduction, SpineError> {
        self.parse_stack
            .prepare_current_task_tree_reduction(archive, memory)
    }

    pub(super) fn next_child_id(&self) -> Result<NodeId, SpineError> {
        self.parse_stack.next_child_id()
    }

    pub(super) fn current_root_epoch_id(&self) -> Result<NodeId, SpineError> {
        self.parse_stack.current_root_epoch_id()
    }

    pub(super) fn root_compact_next_open_index_or_probe(
        &self,
        memory: &MemoryRef,
        next_open_input_tokens: Option<i64>,
        next_open_context_tokens: Option<i64>,
        raw_items: &[Option<ResponseItem>],
        staged_memory_body: Option<(&str, &str)>,
        trim_projection: &TrimProjection,
        archive: &SpineArchive,
    ) -> Result<usize, SpineError> {
        if let Some(next_open_index) = self.parse_stack.pending_compact_next_open_index(
            memory,
            next_open_input_tokens,
            next_open_context_tokens,
        )? {
            return Ok(next_open_index);
        }

        // Probe first because source_context_range records the pre-compact source
        // span, while next_open_index is the post-compact h(PS) materialized len.
        let mut probe_parse_stack = self.parse_stack.clone();
        let token = crate::spine::lexer::plan_root_compact().lex_compact_token(
            memory.clone(),
            0,
            next_open_input_tokens,
            next_open_context_tokens,
        )?;
        probe_parse_stack.shift(token, archive)?;
        Ok(
            render_parse_stack_to_context_with_memory_body_and_trim_projection(
                &probe_parse_stack,
                raw_items,
                staged_memory_body,
                trim_projection,
            )?
            .len(),
        )
    }

    pub(super) fn into_parse_stack(self) -> ParseStack {
        self.parse_stack
    }

    #[cfg(test)]
    pub(super) fn parse_stack_mut_for_runtime_transition(&mut self) -> &mut ParseStack {
        &mut self.parse_stack
    }

    pub(super) fn set_live_open_context_baseline(
        &mut self,
        node: &NodeId,
        input_tokens: i64,
        source: ContextBaselineSource,
    ) -> Result<bool, SpineError> {
        self.parse_stack
            .set_live_open_context_baseline(node, input_tokens, source)
    }

    fn replace_parse_stack_for_runtime_transition(&mut self, state: ParserPreparedState) {
        self.parse_stack = state.into_parse_stack();
    }

    pub(super) fn install_pending_close_after_side_effect_failure(
        &mut self,
        state: ParserPreparedState,
    ) {
        self.replace_parse_stack_for_runtime_transition(state);
    }

    pub(super) fn install_prepared_commit_final_parse_stack(&mut self, state: ParserPreparedState) {
        self.replace_parse_stack_for_runtime_transition(state);
    }

    pub(super) fn install_pending_root_compact_after_side_effect_failure(
        &mut self,
        state: ParserPreparedState,
    ) {
        self.replace_parse_stack_for_runtime_transition(state);
    }

    pub(super) fn install_prepared_root_compact_final_parse_stack(
        &mut self,
        state: ParserPreparedState,
    ) {
        self.replace_parse_stack_for_runtime_transition(state);
    }

    pub(super) fn staged_after_tokens(
        &self,
        tokens: impl IntoIterator<Item = SpineToken>,
        archive: &SpineArchive,
    ) -> Result<ParserPreparedState, SpineError> {
        let mut staged = self.parse_stack.clone();
        for token in tokens {
            staged.shift(token, archive)?;
        }
        Ok(ParserPreparedState::new(staged))
    }

    pub(super) fn open_staged_parse_stack(
        &self,
        open_token: SpineToken,
        toolcall_token: Option<SpineToken>,
        archive: &SpineArchive,
    ) -> Result<ParserPreparedState, SpineError> {
        let mut staged = self.parse_stack.clone();
        staged.shift(open_token, archive)?;
        if let Some(toolcall_token) = toolcall_token {
            staged.shift(toolcall_token, archive)?;
        }
        Ok(ParserPreparedState::new(staged))
    }

    pub(super) fn close_reduced_next_child_id(
        &self,
        memory: MemoryRef,
        reduction: PreparedTaskTreeReduction,
        archive: &SpineArchive,
    ) -> Result<NodeId, SpineError> {
        let mut pending = self.parse_stack.clone();
        pending.shift_pending_close(memory, archive)?;
        let reduced = pending.task_tree_reduced(reduction)?;
        reduced.next_child_id()
    }

    pub(super) fn close_family_staged_parse_stacks(
        &self,
        memory: MemoryRef,
        reduction: PreparedTaskTreeReduction,
        open_token: Option<SpineToken>,
        toolcall_token: SpineToken,
        archive: &SpineArchive,
    ) -> Result<(ParserPreparedState, ParserPreparedState), SpineError> {
        let mut pending = self.parse_stack.clone();
        pending.shift_pending_close(memory, archive)?;
        let mut final_parse_stack = pending.task_tree_reduced(reduction)?;
        if let Some(open_token) = open_token {
            final_parse_stack.shift(open_token, archive)?;
        }
        final_parse_stack.shift(toolcall_token, archive)?;
        Ok((
            ParserPreparedState::new(pending),
            ParserPreparedState::new(final_parse_stack),
        ))
    }

    pub(super) fn prepare_root_compact_reduction(
        &self,
        memory: MemoryRef,
        next_open_index: usize,
        next_open_input_tokens: Option<i64>,
        next_open_context_tokens: Option<i64>,
        raw_items: &[Option<ResponseItem>],
        staged_memory_body: Option<(&str, &str)>,
        trim_projection: &TrimProjection,
        archive: &SpineArchive,
    ) -> Result<ParserRootCompactPreparedReduction, SpineError> {
        let mut pending = self.parse_stack.clone();
        pending.shift_pending_compact(
            memory.clone(),
            next_open_index,
            next_open_input_tokens,
            next_open_context_tokens,
            archive,
        )?;
        let root_epoch_reduction = pending.prepare_root_epoch_reduction(
            archive,
            memory,
            next_open_index,
            next_open_input_tokens,
            next_open_context_tokens,
        )?;
        let final_parse_stack = pending.root_epoch_reduced(root_epoch_reduction.clone())?;
        let materialized = render_parse_stack_to_context_with_memory_body_and_trim_projection(
            &final_parse_stack,
            raw_items,
            staged_memory_body,
            trim_projection,
        )?;
        let current_open_index = final_parse_stack.current_open_meta()?.index;
        Ok(ParserRootCompactPreparedReduction {
            final_parse_stack: ParserPreparedState::new(final_parse_stack),
            root_epoch_reduction,
            materialized,
            current_open_index,
        })
    }

    pub(super) fn root_compact_staged_parse_stacks(
        &self,
        memory: MemoryRef,
        next_open_index: usize,
        next_open_input_tokens: Option<i64>,
        next_open_context_tokens: Option<i64>,
        reduction: PreparedRootEpochReduction,
        archive: &SpineArchive,
    ) -> Result<(ParserPreparedState, ParserPreparedState), SpineError> {
        let mut pending = self.parse_stack.clone();
        pending.shift_pending_compact(
            memory,
            next_open_index,
            next_open_input_tokens,
            next_open_context_tokens,
            archive,
        )?;
        let final_parse_stack = pending.root_epoch_reduced(reduction)?;
        Ok((
            ParserPreparedState::new(pending),
            ParserPreparedState::new(final_parse_stack),
        ))
    }

    pub(super) fn install_staged(&mut self, state: ParserPreparedState) {
        self.parse_stack = state.into_parse_stack();
    }

    pub(super) fn apply_replay_event(
        &mut self,
        event: &LoggedSpineLedgerEvent,
        archive: &SpineArchive,
        mems: &BTreeMap<String, MemRecord>,
        raw_mask: RawMask<'_>,
    ) -> Result<(), SpineError> {
        if !apply_metadata_event(&mut self.parse_stack, event)? {
            let token = event_to_token(event, archive, mems, raw_mask)?;
            self.parse_stack.shift(token, archive)?;
        }
        Ok(())
    }

    pub(super) fn staged_after_lexed_batch_for_observe(
        &self,
        lexed: &LexedTokenBatch,
        archive: &SpineArchive,
    ) -> Result<ParserPreparedState, SpineError> {
        self.staged_after_tokens(lexed.tokens.iter().cloned(), archive)
    }

    pub(super) fn materialize_variable_context(
        &self,
        raw_items: &[Option<ResponseItem>],
        trim_projection: &TrimProjection,
    ) -> Result<Vec<ResponseItem>, SpineError> {
        render_parse_stack_to_context_with_trim_projection(
            &self.parse_stack,
            raw_items,
            trim_projection,
        )
    }

    pub(super) fn build_checkpoint(
        &self,
        rollout_path: &Path,
        raw_ordinal: u64,
        token_seq: u64,
        pressure_seq_watermark: Option<u64>,
        trim_seq_watermark: Option<u64>,
        raw_live: &[bool],
        raw_items: &[Option<ResponseItem>],
        trim_projection: &TrimProjection,
    ) -> Result<SpineCheckpoint, SpineError> {
        let context = self.materialize_variable_context(raw_items, trim_projection)?;
        build_checkpoint(
            rollout_path,
            raw_ordinal,
            token_seq,
            pressure_seq_watermark,
            trim_seq_watermark,
            raw_live,
            &self.parse_stack,
            &context,
        )
    }

    #[cfg(test)]
    pub(super) fn msg_leaf_count_for_test(&self) -> usize {
        parse_stack_msg_leaf_count(&self.parse_stack.symbols)
    }

    #[cfg(test)]
    pub(super) fn toolcall_leaf_count_for_test(&self) -> usize {
        parse_stack_toolcall_leaf_count(&self.parse_stack.symbols)
    }
}
