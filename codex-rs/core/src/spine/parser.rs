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
use crate::spine::model::NodeId;
use crate::spine::model::RawMask;
use crate::spine::model::SpineToken;
use crate::spine::model::SpineTreeNode;
use crate::spine::model::Symbol;
use crate::spine::model::TreeMeta;
use crate::spine::model::TrimProjection;
use crate::spine::parse_stack::ParseStack;
use crate::spine::parse_stack::apply_replay_event_to_parse_stack;
use crate::spine::parse_stack::parse_stack_from_events_with_forced_events;
#[cfg(test)]
use crate::spine::parse_stack::parse_stack_msg_leaf_count;
#[cfg(test)]
use crate::spine::parse_stack::parse_stack_toolcall_leaf_count;
use crate::spine::render::render_parse_stack_to_context_with_trim_projection;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ParserState {
    parse_stack: ParseStack,
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
        parse_stack_from_events_with_forced_events(
            events,
            archive,
            mems,
            raw_mask,
            forced_event_seqs,
            marker_structural_event_seqs,
        )
        .map(Self::from_parse_stack)
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

    pub(super) fn next_child_id(&self) -> Result<NodeId, SpineError> {
        self.parse_stack.next_child_id()
    }

    pub(super) fn into_parse_stack(self) -> ParseStack {
        self.parse_stack
    }

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

    pub(super) fn replace_parse_stack_for_runtime_transition(&mut self, parse_stack: ParseStack) {
        self.parse_stack = parse_stack;
    }

    pub(super) fn staged_after_token(
        &self,
        token: SpineToken,
        archive: &SpineArchive,
    ) -> Result<ParseStack, SpineError> {
        let mut staged = self.parse_stack.clone();
        staged.shift(token, archive)?;
        Ok(staged)
    }

    pub(super) fn staged_after_tokens(
        &self,
        tokens: impl IntoIterator<Item = SpineToken>,
        archive: &SpineArchive,
    ) -> Result<ParseStack, SpineError> {
        let mut staged = self.parse_stack.clone();
        for token in tokens {
            staged.shift(token, archive)?;
        }
        Ok(staged)
    }

    pub(super) fn install_staged(&mut self, parse_stack: ParseStack) {
        self.parse_stack = parse_stack;
    }

    pub(super) fn apply_replay_event(
        &mut self,
        event: &LoggedSpineLedgerEvent,
        archive: &SpineArchive,
        mems: &BTreeMap<String, MemRecord>,
        raw_mask: RawMask<'_>,
    ) -> Result<(), SpineError> {
        apply_replay_event_to_parse_stack(&mut self.parse_stack, event, archive, mems, raw_mask)
    }

    pub(super) fn staged_after_lexed_batch_for_observe(
        &self,
        lexed: &LexedTokenBatch,
        archive: &SpineArchive,
    ) -> Result<ParseStack, SpineError> {
        let mut staged = self.parse_stack.clone();
        for token in &lexed.tokens {
            staged.shift(token.clone(), archive)?;
        }
        Ok(staged)
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
