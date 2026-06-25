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
use crate::spine::model::SpineTreeNode;
use crate::spine::model::Symbol;
use crate::spine::model::ToolCallSegmentKind;
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

pub(super) struct ParserRootCompactPreparedTxn {
    publication: ParserRootCompactPublication,
    prepared_install: ParserRootCompactPreparedInstall,
}

pub(super) struct ParserRootCompactPublication {
    variable_context: Vec<ResponseItem>,
    current_open_index: usize,
}

#[derive(Debug)]
pub(super) struct ParserObserveInstall {
    final_state: ParserPreparedState,
}

#[derive(Debug)]
pub(super) struct ParserOpenInstall {
    final_state: ParserPreparedState,
}

#[derive(Debug)]
pub(super) struct ParserCommitInstall {
    final_state: ParserPreparedState,
}

#[derive(Debug)]
pub(super) struct ParserCommitPreparedInstall {
    pending_install: ParserCommitPendingInstall,
    final_install: ParserCommitInstall,
}

#[derive(Debug)]
pub(super) struct ParserCommitPendingInstall {
    pending_state: ParserPreparedState,
}

#[derive(Debug)]
pub(super) struct ParserRootCompactInstall {
    final_state: ParserPreparedState,
}

#[derive(Debug)]
pub(super) struct ParserRootCompactPreparedInstall {
    pending_install: ParserRootCompactPendingInstall,
    final_install: ParserRootCompactInstall,
}

#[derive(Debug)]
pub(super) struct ParserRootCompactPendingInstall {
    pending_state: ParserPreparedState,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct ParserPublicationPlan {
    operation: &'static str,
    suffix_start: usize,
    replacement_prefix: Vec<ResponseItem>,
    preserve_host_history_from: usize,
    append_current_tool_response_if_missing: bool,
    atomic_mutable_context_segments: Vec<ParserPublicationToolcallSegment>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ParserPublicationToolcallSegment {
    pub(super) kind: ToolCallSegmentKind,
    pub(super) mutable_context_index: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct ParserPublicationUpdate {
    operation: &'static str,
    suffix_start: usize,
    expected_history: Vec<ResponseItem>,
    replacement: Vec<ResponseItem>,
}

impl ParserPublicationUpdate {
    fn new(
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

    pub(super) fn into_history_update<T>(
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
    pub(super) fn suffix_start(&self) -> usize {
        self.suffix_start
    }

    pub(super) fn preserve_host_history_from(&self) -> usize {
        self.preserve_host_history_from
    }

    pub(super) fn atomic_mutable_context_segments(&self) -> &[ParserPublicationToolcallSegment] {
        &self.atomic_mutable_context_segments
    }

    pub(super) fn history_update_with_host_boundaries(
        &self,
        call_id: &str,
        tool_resp_item: &ResponseItem,
        tool_resp_already_recorded: bool,
        host_suffix_start: usize,
        host_preserve_history_from: usize,
        history_items: &[ResponseItem],
    ) -> Result<Option<ParserPublicationUpdate>, SpineError> {
        let suffix_end = history_items.len();
        if host_suffix_start > suffix_end {
            return Err(SpineError::Invariant(format!(
                "{} suffix start {} exceeds history length {suffix_end} for call_id={call_id}",
                self.operation, host_suffix_start
            )));
        }
        if host_preserve_history_from > suffix_end {
            return Err(SpineError::Invariant(format!(
                "{} preserve-host-history index {} exceeds history length {suffix_end} for call_id={call_id}",
                self.operation, host_preserve_history_from
            )));
        }
        let mut replacement = self.replacement_prefix.clone();
        replacement.extend_from_slice(&history_items[host_preserve_history_from..]);
        if self.append_current_tool_response_if_missing && !tool_resp_already_recorded {
            replacement.push(tool_resp_item.clone());
        }
        Ok(Some(ParserPublicationUpdate::new(
            self.operation,
            host_suffix_start,
            history_items.to_vec(),
            replacement,
        )))
    }
}

fn full_variable_context_publication_update(
    operation: &'static str,
    materialized: Vec<ResponseItem>,
    history_items: &[ResponseItem],
) -> Option<ParserPublicationUpdate> {
    if materialized.as_slice() == history_items {
        return None;
    }
    Some(ParserPublicationUpdate::new(
        operation,
        0,
        history_items.to_vec(),
        materialized,
    ))
}

impl ParserRootCompactPreparedTxn {
    pub(super) fn validate_current_open_matches_variable_context_len(
        &self,
    ) -> Result<(), SpineError> {
        self.publication
            .validate_current_open_matches_variable_context_len()
    }

    pub(super) fn into_variable_context_and_install(
        self,
    ) -> (
        Vec<ResponseItem>,
        ParserRootCompactPendingInstall,
        ParserRootCompactInstall,
    ) {
        let (pending_install, final_install) = self.prepared_install.into_parts();
        (
            self.publication.into_variable_context(),
            pending_install,
            final_install,
        )
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
            self.prepared_install.final_state().parse_stack(),
            self.publication.variable_context(),
            self.publication.variable_context(),
        )
    }
}

impl ParserRootCompactPublication {
    fn new(variable_context: Vec<ResponseItem>, current_open_index: usize) -> Self {
        Self {
            variable_context,
            current_open_index,
        }
    }

    fn variable_context(&self) -> &[ResponseItem] {
        &self.variable_context
    }

    fn validate_current_open_matches_variable_context_len(&self) -> Result<(), SpineError> {
        if self.current_open_index != self.variable_context.len() {
            return Err(SpineError::Invariant(format!(
                "spine root compact open index {} does not match variable context length {}",
                self.current_open_index,
                self.variable_context.len()
            )));
        }
        Ok(())
    }

    fn into_variable_context(self) -> Vec<ResponseItem> {
        self.variable_context
    }
}

impl ParserObserveInstall {
    fn new(final_state: ParserPreparedState) -> Self {
        Self { final_state }
    }

    fn into_final_state(self) -> ParserPreparedState {
        self.final_state
    }
}

impl ParserCommitInstall {
    fn new(final_state: ParserPreparedState) -> Self {
        Self { final_state }
    }

    fn into_final_state(self) -> ParserPreparedState {
        self.final_state
    }

    pub(super) fn full_variable_context_host_history_update<T>(
        &self,
        call_id: &str,
        operation: &'static str,
        raw_items: &[Option<ResponseItem>],
        trim_projection: &TrimProjection,
        history_items: &[ResponseItem],
        build_update: impl FnOnce(&str, &'static str, usize, Vec<ResponseItem>, Vec<ResponseItem>) -> T,
    ) -> Result<Option<T>, SpineError> {
        let materialized = self
            .final_state
            .materialize_variable_context(raw_items, trim_projection)?;
        Ok(full_variable_context_publication_update(
            operation,
            materialized,
            history_items,
        ))
        .map(|update| update.map(|update| update.into_history_update(call_id, build_update)))
    }
}

impl ParserCommitPendingInstall {
    fn new(pending_state: ParserPreparedState) -> Self {
        Self { pending_state }
    }
}

impl ParserCommitPreparedInstall {
    fn new(
        pending_install: ParserCommitPendingInstall,
        final_install: ParserCommitInstall,
    ) -> Self {
        Self {
            pending_install,
            final_install,
        }
    }

    pub(super) fn pending_install(&self) -> &ParserCommitPendingInstall {
        &self.pending_install
    }

    pub(super) fn into_final_install(self) -> ParserCommitInstall {
        self.final_install
    }
}

impl ParserOpenInstall {
    fn new(final_state: ParserPreparedState) -> Self {
        Self { final_state }
    }

    fn into_final_state(self) -> ParserPreparedState {
        self.final_state
    }

    pub(super) fn into_commit_install(self) -> ParserCommitInstall {
        ParserCommitInstall::new(self.final_state)
    }
}

impl ParserRootCompactInstall {
    fn new(final_state: ParserPreparedState) -> Self {
        Self { final_state }
    }

    fn into_final_state(self) -> ParserPreparedState {
        self.final_state
    }
}

impl ParserRootCompactPreparedInstall {
    fn new(
        pending_install: ParserRootCompactPendingInstall,
        final_install: ParserRootCompactInstall,
    ) -> Self {
        Self {
            pending_install,
            final_install,
        }
    }

    fn final_state(&self) -> &ParserPreparedState {
        &self.final_install.final_state
    }

    fn into_parts(self) -> (ParserRootCompactPendingInstall, ParserRootCompactInstall) {
        (self.pending_install, self.final_install)
    }
}

impl ParserRootCompactPendingInstall {
    fn new(pending_state: ParserPreparedState) -> Self {
        Self { pending_state }
    }

    fn into_pending_state(self) -> ParserPreparedState {
        self.pending_state
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

    fn materialize_variable_context(
        &self,
        raw_items: &[Option<ResponseItem>],
        trim_projection: &TrimProjection,
    ) -> Result<Vec<ResponseItem>, SpineError> {
        materialize_parse_stack_variable_context(self.parse_stack(), raw_items, trim_projection)
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

    pub(super) fn last_visible_response_context_index(&self) -> Option<usize> {
        self.parse_stack.last_visible_response_context_index()
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
        atomic_mutable_context_segments: Vec<ParserPublicationToolcallSegment>,
    ) -> ParserPublicationPlan {
        ParserPublicationPlan {
            operation,
            suffix_start,
            replacement_prefix,
            preserve_host_history_from,
            append_current_tool_response_if_missing: true,
            atomic_mutable_context_segments,
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
        let probe_batch = crate::spine::lexer::plan_root_compact().lex_compact_batch(
            memory.clone(),
            0,
            next_open_input_tokens,
            next_open_context_tokens,
        )?;
        let probe_state = self.stage_lexed_batches(std::iter::once(&probe_batch), archive)?;
        Ok(
            render_parse_stack_to_context_with_memory_body_and_trim_projection(
                probe_state.parse_stack(),
                raw_items,
                staged_memory_body,
                trim_projection,
            )?
            .len(),
        )
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

    fn install_prepared_state(&mut self, state: ParserPreparedState) {
        self.parse_stack = state.into_parse_stack();
    }

    pub(super) fn install_pending_close_after_side_effect_failure(
        &mut self,
        install: &ParserCommitPendingInstall,
    ) {
        self.parse_stack = install.pending_state.parse_stack.clone();
    }

    pub(super) fn install_prepared_commit(&mut self, install: ParserCommitInstall) {
        self.install_prepared_state(install.into_final_state());
    }

    pub(super) fn install_prepared_observe(&mut self, install: ParserObserveInstall) {
        self.install_prepared_state(install.into_final_state());
    }

    pub(super) fn install_prepared_open(&mut self, install: ParserOpenInstall) {
        self.install_prepared_state(install.into_final_state());
    }

    pub(super) fn install_pending_root_compact_after_side_effect_failure(
        &mut self,
        install: ParserRootCompactPendingInstall,
    ) {
        self.install_prepared_state(install.into_pending_state());
    }

    pub(super) fn install_prepared_root_compact(&mut self, install: ParserRootCompactInstall) {
        self.install_prepared_state(install.into_final_state());
    }

    fn stage_lexed_batches<'a>(
        &self,
        batches: impl IntoIterator<Item = &'a LexedTokenBatch>,
        archive: &SpineArchive,
    ) -> Result<ParserPreparedState, SpineError> {
        let mut staged = self.parse_stack.clone();
        apply_lexed_batches_to_parse_stack(&mut staged, batches, archive)?;
        Ok(ParserPreparedState::new(staged))
    }

    pub(super) fn prepare_open_install(
        &self,
        open_lexed: &LexedTokenBatch,
        toolcall_lexed: Option<&LexedTokenBatch>,
        archive: &SpineArchive,
    ) -> Result<ParserOpenInstall, SpineError> {
        let batches = std::iter::once(open_lexed).chain(toolcall_lexed);
        let staged = self.stage_lexed_batches(batches, archive)?;
        Ok(ParserOpenInstall::new(staged))
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

    pub(super) fn prepare_close_family_install(
        &self,
        memory: MemoryRef,
        reduction: PreparedTaskTreeReduction,
        open_lexed: Option<&LexedTokenBatch>,
        toolcall_lexed: &LexedTokenBatch,
        archive: &SpineArchive,
    ) -> Result<ParserCommitPreparedInstall, SpineError> {
        let mut pending = self.parse_stack.clone();
        pending.shift_pending_close(memory, archive)?;
        let mut final_parse_stack = pending.task_tree_reduced(reduction)?;
        let final_batches = open_lexed
            .into_iter()
            .chain(std::iter::once(toolcall_lexed));
        apply_lexed_batches_to_parse_stack(&mut final_parse_stack, final_batches, archive)?;
        Ok(ParserCommitPreparedInstall::new(
            ParserCommitPendingInstall::new(ParserPreparedState::new(pending)),
            ParserCommitInstall::new(ParserPreparedState::new(final_parse_stack)),
        ))
    }

    pub(super) fn prepare_root_compact_txn(
        &self,
        memory: MemoryRef,
        next_open_index: usize,
        next_open_input_tokens: Option<i64>,
        next_open_context_tokens: Option<i64>,
        raw_items: &[Option<ResponseItem>],
        staged_memory_body: Option<(&str, &str)>,
        trim_projection: &TrimProjection,
        archive: &SpineArchive,
    ) -> Result<ParserRootCompactPreparedTxn, SpineError> {
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
        let variable_context = render_parse_stack_to_context_with_memory_body_and_trim_projection(
            &final_parse_stack,
            raw_items,
            staged_memory_body,
            trim_projection,
        )?;
        let current_open_index = final_parse_stack.current_open_meta()?.index;
        Ok(ParserRootCompactPreparedTxn {
            publication: ParserRootCompactPublication::new(variable_context, current_open_index),
            prepared_install: ParserRootCompactPreparedInstall::new(
                ParserRootCompactPendingInstall::new(ParserPreparedState::new(pending)),
                ParserRootCompactInstall::new(ParserPreparedState::new(final_parse_stack)),
            ),
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
            let lexed = replay_event_to_lexed_batch(event, archive, mems, raw_mask)?;
            let staged = self.stage_lexed_batches(std::iter::once(&lexed), archive)?;
            self.install_prepared_state(staged);
        }
        Ok(())
    }

    pub(super) fn consume_lexed_batch(
        &self,
        lexed: &LexedTokenBatch,
        archive: &SpineArchive,
    ) -> Result<ParserObserveInstall, SpineError> {
        let staged = self.stage_lexed_batches(std::iter::once(lexed), archive)?;
        Ok(ParserObserveInstall::new(staged))
    }

    pub(super) fn materialize_variable_context(
        &self,
        raw_items: &[Option<ResponseItem>],
        trim_projection: &TrimProjection,
    ) -> Result<Vec<ResponseItem>, SpineError> {
        materialize_parse_stack_variable_context(&self.parse_stack, raw_items, trim_projection)
    }

    pub(super) fn full_variable_context_host_history_update<T>(
        &self,
        call_id: &str,
        operation: &'static str,
        raw_items: &[Option<ResponseItem>],
        trim_projection: &TrimProjection,
        history_items: &[ResponseItem],
        build_update: impl FnOnce(&str, &'static str, usize, Vec<ResponseItem>, Vec<ResponseItem>) -> T,
    ) -> Result<Option<T>, SpineError> {
        let materialized = self.materialize_variable_context(raw_items, trim_projection)?;
        Ok(full_variable_context_publication_update(
            operation,
            materialized,
            history_items,
        ))
        .map(|update| update.map(|update| update.into_history_update(call_id, build_update)))
    }

    pub(super) fn variable_context_len(
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

    pub(super) fn validate_checkpoint_parse_stack(
        &self,
        checkpoint: &SpineCheckpoint,
    ) -> Result<(), SpineError> {
        if self.parse_stack != checkpoint.parse_stack {
            return Err(SpineError::Invariant(format!(
                "spine checkpoint ParseStack mismatch for {} at raw_ordinal={} token_seq={}",
                checkpoint.checkpoint_id, checkpoint.raw_ordinal, checkpoint.token_seq
            )));
        }
        Ok(())
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

fn replay_event_to_lexed_batch(
    event: &LoggedSpineLedgerEvent,
    archive: &SpineArchive,
    mems: &BTreeMap<String, MemRecord>,
    raw_mask: RawMask<'_>,
) -> Result<LexedTokenBatch, SpineError> {
    match &event.event {
        SpineLedgerEvent::Init { raw_start } => crate::spine::lexer::lex_init(archive, *raw_start),
        SpineLedgerEvent::Msg {
            raw_ordinal,
            context_index,
            from_user,
            user_anchor,
        } => crate::spine::lexer::lex_msg(*raw_ordinal, *context_index, *from_user, *user_anchor),
        SpineLedgerEvent::ToolCall { segments } => {
            crate::spine::lexer::lex_toolcall_event(segments.iter().cloned())
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
        } => crate::spine::lexer::lex_open(
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
            let SpineLedgerEvent::Close {
                node,
                boundary,
                summary,
                close_input_tokens,
                close_context_tokens,
            } = &event.event
            else {
                unreachable!("close event was matched before replay close lexing")
            };
            crate::spine::lexer::lex_close(
                node.clone(),
                *boundary,
                summary.clone(),
                *close_input_tokens,
                *close_context_tokens,
                replay_memory_ref(archive, mem, event.seq),
            )
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
            crate::spine::lexer::plan_root_compact().lex_compact_batch(
                memory,
                usize::try_from(*next_open_index).map_err(|_| {
                    SpineError::InvalidEvent("root open index overflow".to_string())
                })?,
                None,
                None,
            )
        }
        SpineLedgerEvent::OpenContextBaseline { .. } => Err(SpineError::InvalidEvent(
            "OpenContextBaseline is metadata and cannot be converted to a LexedTokenBatch"
                .to_string(),
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

fn apply_lexed_batches_to_parse_stack<'a>(
    parse_stack: &mut ParseStack,
    batches: impl IntoIterator<Item = &'a LexedTokenBatch>,
    archive: &SpineArchive,
) -> Result<(), SpineError> {
    for batch in batches {
        for token in batch.tokens.iter().cloned() {
            parse_stack.shift(token, archive)?;
        }
    }
    Ok(())
}

fn materialize_parse_stack_variable_context(
    parse_stack: &ParseStack,
    raw_items: &[Option<ResponseItem>],
    trim_projection: &TrimProjection,
) -> Result<Vec<ResponseItem>, SpineError> {
    render_parse_stack_to_context_with_trim_projection(parse_stack, raw_items, trim_projection)
}
