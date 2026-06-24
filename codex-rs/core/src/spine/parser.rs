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
use codex_protocol::spine_tree::SpineTreeUpdateEvent;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;

use crate::spine::SpineError;
use crate::spine::archive::SpineArchive;
use crate::spine::archive::memory_ref;
use crate::spine::checkpoint::SpineCheckpoint;
use crate::spine::checkpoint::build_checkpoint;
use crate::spine::compact_checkpoint::SpineCompactCheckpoint;
use crate::spine::compact_checkpoint::build_compact_checkpoint;
use crate::spine::lexer::LexedTokenBatch;
use crate::spine::model::ContextBaselineSource;
use crate::spine::model::ControlSymbol;
use crate::spine::model::LoggedSpineLedgerEvent;
use crate::spine::model::MemRecord;
use crate::spine::model::MemoryRef;
use crate::spine::model::NodeId;
use crate::spine::model::RawMask;
use crate::spine::model::SpineLedgerEvent;
use crate::spine::model::SpineToken;
use crate::spine::model::SpineTreeNode;
use crate::spine::model::Symbol;
use crate::spine::model::TreeMeta;
use crate::spine::model::TrimProjection;
use crate::spine::parse_stack::ParseStack;
use crate::spine::parse_stack::PreparedTaskTreeReduction;
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
    materialized: Vec<ResponseItem>,
    current_open_index: usize,
    pending_install: ParserRootCompactPendingInstall,
    parser_install: ParserRootCompactInstall,
}

#[derive(Debug)]
pub(super) struct ParserObserveInstall {
    final_parse_stack: ParserPreparedState,
}

#[derive(Debug)]
pub(super) struct ParserOpenInstall {
    final_parse_stack: ParserPreparedState,
}

#[derive(Debug)]
pub(super) struct ParserCommitInstall {
    final_parse_stack: ParserPreparedState,
}

#[derive(Debug)]
pub(super) struct ParserCommitPendingInstall {
    pending_parse_stack: ParserPreparedState,
}

#[derive(Debug)]
pub(super) struct ParserRootCompactInstall {
    final_parse_stack: ParserPreparedState,
}

#[derive(Debug)]
pub(super) struct ParserRootCompactPendingInstall {
    pending_parse_stack: ParserPreparedState,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ParserPublicationPlan {
    operation: &'static str,
    suffix_start: usize,
    replacement_prefix: Vec<ResponseItem>,
    preserve_host_history_from: usize,
    append_current_tool_response_if_missing: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ParserPublicationUpdate {
    operation: &'static str,
    suffix_start: usize,
    expected_history: Vec<ResponseItem>,
    replacement: Vec<ResponseItem>,
}

impl ParserPublicationUpdate {
    pub(crate) fn new(
        operation: &'static str,
        suffix_start: usize,
        expected_history: Vec<ResponseItem>,
        replacement: Vec<ResponseItem>,
    ) -> Self {
        Self {
            operation,
            suffix_start,
            expected_history,
            replacement,
        }
    }

    pub(crate) fn into_history_update<T>(
        self,
        call_id: &str,
        build_update: impl FnOnce(&str, &'static str, usize, Vec<ResponseItem>, Vec<ResponseItem>) -> T,
    ) -> T {
        build_update(
            call_id,
            self.operation,
            self.suffix_start,
            self.expected_history,
            self.replacement,
        )
    }
}

impl ParserPublicationPlan {
    pub(super) fn history_update(
        &self,
        call_id: &str,
        tool_resp_item: &ResponseItem,
        tool_resp_already_recorded: bool,
        history_items: &[ResponseItem],
    ) -> Result<Option<ParserPublicationUpdate>, SpineError> {
        let suffix_end = history_items.len();
        if self.suffix_start > suffix_end {
            return Err(SpineError::Invariant(format!(
                "{} suffix start {} exceeds history length {suffix_end} for call_id={call_id}",
                self.operation, self.suffix_start
            )));
        }
        if self.preserve_host_history_from > suffix_end {
            return Err(SpineError::Invariant(format!(
                "{} preserve-host-history index {} exceeds history length {suffix_end} for call_id={call_id}",
                self.operation, self.preserve_host_history_from
            )));
        }
        let mut replacement = self.replacement_prefix.clone();
        replacement.extend_from_slice(&history_items[self.preserve_host_history_from..]);
        if self.append_current_tool_response_if_missing && !tool_resp_already_recorded {
            replacement.push(tool_resp_item.clone());
        }
        Ok(Some(ParserPublicationUpdate::new(
            self.operation,
            self.suffix_start,
            history_items.to_vec(),
            replacement,
        )))
    }

    #[cfg(test)]
    pub(crate) fn operation(&self) -> &'static str {
        self.operation
    }

    #[cfg(test)]
    pub(crate) fn suffix_start(&self) -> usize {
        self.suffix_start
    }

    #[cfg(test)]
    pub(crate) fn replacement_prefix(&self) -> &[ResponseItem] {
        &self.replacement_prefix
    }

    #[cfg(test)]
    pub(crate) fn preserve_host_history_from(&self) -> usize {
        self.preserve_host_history_from
    }

    #[cfg(test)]
    pub(crate) fn append_current_tool_response_if_missing(&self) -> bool {
        self.append_current_tool_response_if_missing
    }
}

impl ParserRootCompactPreparedReduction {
    pub(super) fn validate_current_open_matches_materialized_len(&self) -> Result<(), SpineError> {
        if self.current_open_index != self.materialized.len() {
            return Err(SpineError::Invariant(format!(
                "spine root compact open index {} does not match materialized history length {}",
                self.current_open_index,
                self.materialized.len()
            )));
        }
        Ok(())
    }

    pub(super) fn into_materialized_and_install(
        self,
    ) -> (
        Vec<ResponseItem>,
        ParserRootCompactPendingInstall,
        ParserRootCompactInstall,
    ) {
        (self.materialized, self.pending_install, self.parser_install)
    }

    pub(super) fn build_compact_checkpoint(
        &self,
        rollout_path: &Path,
        raw_boundary: u64,
        token_seq: u64,
        raw_live: &[bool],
        raw_items: &[Option<ResponseItem>],
    ) -> Result<SpineCompactCheckpoint, SpineError> {
        build_compact_checkpoint(
            rollout_path,
            raw_boundary,
            token_seq,
            raw_live,
            raw_items,
            self.parser_install.final_parse_stack.parse_stack(),
            &self.materialized,
            &self.materialized,
        )
    }
}

impl ParserObserveInstall {
    fn new(final_parse_stack: ParserPreparedState) -> Self {
        Self { final_parse_stack }
    }

    fn into_final_parse_stack(self) -> ParserPreparedState {
        self.final_parse_stack
    }
}

impl ParserCommitInstall {
    fn new(final_parse_stack: ParserPreparedState) -> Self {
        Self { final_parse_stack }
    }

    fn into_final_parse_stack(self) -> ParserPreparedState {
        self.final_parse_stack
    }

    pub(super) fn full_context_publication_update(
        &self,
        operation: &'static str,
        raw_items: &[Option<ResponseItem>],
        trim_projection: &TrimProjection,
        history_items: &[ResponseItem],
    ) -> Result<Option<ParserPublicationUpdate>, SpineError> {
        let materialized = render_parse_stack_to_context_with_trim_projection(
            self.final_parse_stack.parse_stack(),
            raw_items,
            trim_projection,
        )?;
        if materialized.as_slice() == history_items {
            return Ok(None);
        }
        Ok(Some(ParserPublicationUpdate::new(
            operation,
            0,
            history_items.to_vec(),
            materialized,
        )))
    }
}

impl ParserCommitPendingInstall {
    fn new(pending_parse_stack: ParserPreparedState) -> Self {
        Self {
            pending_parse_stack,
        }
    }

    fn into_pending_parse_stack(self) -> ParserPreparedState {
        self.pending_parse_stack
    }
}

impl ParserOpenInstall {
    fn new(final_parse_stack: ParserPreparedState) -> Self {
        Self { final_parse_stack }
    }

    fn into_final_parse_stack(self) -> ParserPreparedState {
        self.final_parse_stack
    }

    pub(super) fn into_commit_install(self) -> ParserCommitInstall {
        ParserCommitInstall::new(self.final_parse_stack)
    }
}

impl ParserRootCompactInstall {
    fn new(final_parse_stack: ParserPreparedState) -> Self {
        Self { final_parse_stack }
    }

    fn into_final_parse_stack(self) -> ParserPreparedState {
        self.final_parse_stack
    }
}

impl ParserRootCompactPendingInstall {
    fn new(pending_parse_stack: ParserPreparedState) -> Self {
        Self {
            pending_parse_stack,
        }
    }

    fn into_pending_parse_stack(self) -> ParserPreparedState {
        self.pending_parse_stack
    }
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

    #[cfg(test)]
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

    #[cfg(test)]
    pub(super) fn render_tree_with_memory_context_accounting(
        &self,
        accounting: &BTreeMap<String, i64>,
    ) -> Result<String, SpineError> {
        self.parse_stack_with_memory_context_accounting(accounting)
            .render_tree()
    }

    pub(super) fn render_tree_with_context_annotations_and_memory_context_accounting(
        &self,
        annotations: &BTreeMap<NodeId, String>,
        accounting: &BTreeMap<String, i64>,
    ) -> Result<String, SpineError> {
        self.parse_stack_with_memory_context_accounting(accounting)
            .render_tree_with_context_annotations(annotations)
    }

    pub(super) fn build_tree_snapshot_with_memory_context_accounting(
        &self,
        snapshot_seq: u64,
        accounting: &BTreeMap<String, i64>,
    ) -> Result<SpineTreeUpdateEvent, SpineError> {
        let parse_stack = self.parse_stack_with_memory_context_accounting(accounting);
        let nodes = parse_stack.tree_snapshot_nodes()?;
        let active_node_id = parse_stack.current_cursor_id()?.as_path();
        Ok(SpineTreeUpdateEvent {
            snapshot_seq,
            active_node_id,
            nodes,
        })
    }

    pub(super) fn current_open_meta_cloned(&self) -> Option<TreeMeta> {
        self.parse_stack.current_open_meta_opt().cloned()
    }

    #[cfg(test)]
    pub(super) fn current_open_index(&self) -> Result<usize, SpineError> {
        Ok(self.parse_stack.current_open_meta()?.index)
    }

    #[cfg(test)]
    pub(super) fn current_open_input_tokens(&self) -> Option<i64> {
        self.parse_stack
            .current_open_meta_opt()
            .and_then(|meta| meta.open_input_tokens)
    }

    pub(super) fn current_close_open_meta(&self) -> Result<&TreeMeta, SpineError> {
        let Some(open_meta) = self.parse_stack.current_open_meta_opt() else {
            let cursor = self.parse_stack.current_cursor_id()?;
            if cursor.is_root_epoch() {
                return Err(SpineError::Operation(format!(
                    "cannot close root epoch cursor {cursor}"
                )));
            }
            return Err(SpineError::Operation(
                "spine.close requires a live open task".to_string(),
            ));
        };
        if open_meta.id.is_root_epoch() {
            return Err(SpineError::Operation("cannot close root epoch".to_string()));
        }
        Ok(open_meta)
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

    pub(super) fn close_family_publication_plan(
        &self,
        operation: &'static str,
        suffix_start: usize,
        replacement_prefix: Vec<ResponseItem>,
        preserve_host_history_from: usize,
    ) -> ParserPublicationPlan {
        ParserPublicationPlan {
            operation,
            suffix_start,
            replacement_prefix,
            preserve_host_history_from,
            append_current_tool_response_if_missing: true,
        }
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
    pub(super) fn parse_stack_mut_for_test(&mut self) -> &mut ParseStack {
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
        install: ParserCommitPendingInstall,
    ) {
        self.replace_parse_stack_for_runtime_transition(install.into_pending_parse_stack());
    }

    pub(super) fn install_prepared_commit(&mut self, install: ParserCommitInstall) {
        self.replace_parse_stack_for_runtime_transition(install.into_final_parse_stack());
    }

    pub(super) fn install_prepared_observe(&mut self, install: ParserObserveInstall) {
        self.replace_parse_stack_for_runtime_transition(install.into_final_parse_stack());
    }

    pub(super) fn install_prepared_open(&mut self, install: ParserOpenInstall) {
        self.replace_parse_stack_for_runtime_transition(install.into_final_parse_stack());
    }

    pub(super) fn install_pending_root_compact_after_side_effect_failure(
        &mut self,
        install: ParserRootCompactPendingInstall,
    ) {
        self.replace_parse_stack_for_runtime_transition(install.into_pending_parse_stack());
    }

    pub(super) fn install_prepared_root_compact(&mut self, install: ParserRootCompactInstall) {
        self.replace_parse_stack_for_runtime_transition(install.into_final_parse_stack());
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

    pub(super) fn prepare_open_install(
        &self,
        open_lexed: &LexedTokenBatch,
        toolcall_lexed: Option<&LexedTokenBatch>,
        archive: &SpineArchive,
    ) -> Result<ParserOpenInstall, SpineError> {
        let mut staged = self.parse_stack.clone();
        let open_token = single_lexed_token(open_lexed, "open")?;
        staged.shift(open_token, archive)?;
        if let Some(toolcall_lexed) = toolcall_lexed {
            let toolcall_token = single_lexed_token(toolcall_lexed, "toolcall")?;
            staged.shift(toolcall_token, archive)?;
        }
        Ok(ParserOpenInstall::new(ParserPreparedState::new(staged)))
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
        open_lexed: Option<&LexedTokenBatch>,
        toolcall_lexed: &LexedTokenBatch,
        archive: &SpineArchive,
    ) -> Result<(ParserCommitPendingInstall, ParserCommitInstall), SpineError> {
        let mut pending = self.parse_stack.clone();
        pending.shift_pending_close(memory, archive)?;
        let mut final_parse_stack = pending.task_tree_reduced(reduction)?;
        if let Some(open_lexed) = open_lexed {
            let open_token = single_lexed_token(open_lexed, "open")?;
            final_parse_stack.shift(open_token, archive)?;
        }
        let toolcall_token = single_lexed_token(toolcall_lexed, "toolcall")?;
        final_parse_stack.shift(toolcall_token, archive)?;
        Ok((
            ParserCommitPendingInstall::new(ParserPreparedState::new(pending)),
            ParserCommitInstall::new(ParserPreparedState::new(final_parse_stack)),
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
            materialized,
            current_open_index,
            pending_install: ParserRootCompactPendingInstall::new(ParserPreparedState::new(
                pending,
            )),
            parser_install: ParserRootCompactInstall::new(ParserPreparedState::new(
                final_parse_stack,
            )),
        })
    }

    pub(super) fn apply_replay_event(
        &mut self,
        event: &LoggedSpineLedgerEvent,
        archive: &SpineArchive,
        mems: &BTreeMap<String, MemRecord>,
        raw_mask: RawMask<'_>,
    ) -> Result<(), SpineError> {
        if !apply_replay_metadata_event(&mut self.parse_stack, event)? {
            let token = replay_event_to_token(event, archive, mems, raw_mask)?;
            self.parse_stack.shift(token, archive)?;
        }
        Ok(())
    }

    pub(super) fn prepare_observe_install(
        &self,
        lexed: &LexedTokenBatch,
        archive: &SpineArchive,
    ) -> Result<ParserObserveInstall, SpineError> {
        let staged = self.staged_after_tokens(lexed.tokens.iter().cloned(), archive)?;
        Ok(ParserObserveInstall::new(staged))
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

    pub(super) fn full_variable_context_publication_update(
        &self,
        operation: &'static str,
        raw_items: &[Option<ResponseItem>],
        trim_projection: &TrimProjection,
        history_items: &[ResponseItem],
    ) -> Result<Option<ParserPublicationUpdate>, SpineError> {
        let materialized = self.materialize_variable_context(raw_items, trim_projection)?;
        if materialized.as_slice() == history_items {
            return Ok(None);
        }
        Ok(Some(ParserPublicationUpdate::new(
            operation,
            0,
            history_items.to_vec(),
            materialized,
        )))
    }

    pub(super) fn materialized_variable_context_len(
        &self,
        raw_items: &[Option<ResponseItem>],
        trim_projection: &TrimProjection,
    ) -> Result<usize, SpineError> {
        Ok(self
            .materialize_variable_context(raw_items, trim_projection)?
            .len())
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

    #[cfg(test)]
    pub(super) fn debug_for_test(&self) -> String {
        format!("{:?}", self.parse_stack)
    }
}

fn replay_event_to_token(
    event: &LoggedSpineLedgerEvent,
    archive: &SpineArchive,
    mems: &BTreeMap<String, MemRecord>,
    raw_mask: RawMask<'_>,
) -> Result<SpineToken, SpineError> {
    match &event.event {
        SpineLedgerEvent::Init { raw_start } => {
            crate::spine::lexer::lex_init_token(archive, *raw_start)
        }
        SpineLedgerEvent::Msg {
            raw_ordinal,
            context_index,
            from_user,
            user_anchor,
        } => crate::spine::lexer::lex_msg_token(
            *raw_ordinal,
            *context_index,
            *from_user,
            *user_anchor,
        ),
        SpineLedgerEvent::ToolCall { segments } => {
            crate::spine::lexer::lex_toolcall_event_as_token(segments.iter().cloned())
        }
        SpineLedgerEvent::Open {
            child,
            boundary,
            index,
            summary,
            open_input_tokens,
            open_context_tokens,
            open_context_source,
            ..
        } => crate::spine::lexer::lex_open_token(
            archive,
            child.clone(),
            *boundary,
            *index,
            summary.clone(),
            *open_input_tokens,
            *open_context_tokens,
            *open_context_source,
        ),
        SpineLedgerEvent::Close { node, .. } => {
            let mem = mems.values().find(|mem| &mem.node == node).ok_or_else(|| {
                SpineError::InvalidEvent(format!("missing memory for close node {node}"))
            })?;
            validate_replay_memory_raw_evidence(mem, raw_mask)?;
            crate::spine::lexer::lex_close_token(replay_memory_ref(archive, mem, event.seq))
        }
        SpineLedgerEvent::RootCompact {
            mem,
            next_open_index,
            ..
        } => {
            let mem = mems.get(mem).ok_or_else(|| {
                SpineError::InvalidEvent("missing memory for root compact".to_string())
            })?;
            validate_replay_memory_raw_evidence(mem, raw_mask)?;
            let memory = replay_memory_ref(archive, mem, event.seq);
            crate::spine::lexer::plan_root_compact().lex_compact_token(
                memory,
                usize::try_from(*next_open_index).map_err(|_| {
                    SpineError::InvalidEvent("root open index overflow".to_string())
                })?,
                None,
                None,
            )
        }
        SpineLedgerEvent::OpenContextBaseline { .. } => Err(SpineError::InvalidEvent(
            "OpenContextBaseline is metadata and cannot be converted to a SpineToken".to_string(),
        )),
    }
}

fn replay_memory_ref(archive: &SpineArchive, mem: &MemRecord, event_seq: u64) -> MemoryRef {
    memory_ref(
        archive,
        mem.compact_id.clone(),
        mem.node.clone(),
        mem.body_hash.clone(),
        mem.raw_start..mem.raw_end,
        mem.context_start..mem.context_end,
        event_seq..event_seq + 1,
        mem.open_input_tokens,
        mem.close_input_tokens,
        mem.open_context_tokens,
        mem.close_context_tokens,
        mem.closed_source_suffix_tokens,
        mem.closed_memory_context_tokens,
        mem.open_context_source,
        mem.memory_output_tokens,
    )
}

fn validate_replay_memory_raw_evidence(
    mem: &MemRecord,
    raw_mask: RawMask<'_>,
) -> Result<(), SpineError> {
    if !mem.allowed_by(raw_mask)? {
        return Err(SpineError::InvalidEvent(format!(
            "memory {} does not cover live raw evidence",
            mem.compact_id
        )));
    }
    Ok(())
}

fn apply_replay_metadata_event(
    ps: &mut ParseStack,
    event: &LoggedSpineLedgerEvent,
) -> Result<bool, SpineError> {
    match &event.event {
        SpineLedgerEvent::OpenContextBaseline {
            node,
            open_input_tokens,
            open_context_tokens,
            open_context_source,
            ..
        } => {
            if open_input_tokens != open_context_tokens {
                return Err(SpineError::InvalidEvent(format!(
                    "open context baseline for node {node} has mismatched provider input encoding"
                )));
            }
            ps.set_live_open_context_baseline(node, *open_input_tokens, *open_context_source)
        }
        _ => Ok(false),
    }
}

fn single_lexed_token(lexed: &LexedTokenBatch, label: &str) -> Result<SpineToken, SpineError> {
    let mut tokens = lexed.tokens.iter().cloned();
    let token = tokens
        .next()
        .ok_or_else(|| SpineError::Invariant(format!("{label} lexer produced no token")))?;
    if tokens.next().is_some() {
        return Err(SpineError::Invariant(format!(
            "{label} lexer produced multiple tokens"
        )));
    }
    Ok(token)
}
