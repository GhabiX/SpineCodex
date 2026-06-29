use codex_protocol::models::ResponseItem;
use codex_protocol::spine_tree::SpineTreeUpdateEvent;
use std::collections::BTreeMap;
use std::path::Path;

use crate::spine::SpineError;
use crate::spine::archive::SpineArchive;
use crate::spine::checkpoint::SpineCheckpoint;
use crate::spine::checkpoint::build_checkpoint;
use crate::spine::lexer::LexedTokenBatch;
use crate::spine::model::ContextBaselineSource;
use crate::spine::model::ControlSymbol;
use crate::spine::model::MemoryRef;
use crate::spine::model::NodeId;
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

use super::publication::ParserPublicationPlan;
use super::publication::ParserPublicationToolcallSegmentEvidence;
use super::publication::checkpoint_publication_proof_from_parse_stack;
use super::publication::close_family_publication_plan;
use super::publication::materialize_variable_context_from_state;
use super::publication::root_compact_probe_variable_context_len;
use super::publication::root_compact_publication_from_state;
use super::reducer::apply_lexed_batches_to_parse_stack;
use super::transaction::ParserCommitInstall;
use super::transaction::ParserCommitPendingInstall;
use super::transaction::ParserCommitPreparedInstall;
use super::transaction::ParserObserveInstall;
use super::transaction::ParserOpenInstall;
use super::transaction::ParserPreparedState;
use super::transaction::ParserRootCompactInstall;
use super::transaction::ParserRootCompactPendingInstall;
use super::transaction::ParserRootCompactPreparedCommitInstall;
use super::transaction::ParserRootCompactPreparedInstall;
use super::transaction::ParserRootCompactPreparedTxn;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::spine) struct ParserState {
    pub(in crate::spine::parser) parse_stack: ParseStack,
}

impl ParserState {
    pub(in crate::spine) fn new() -> Self {
        Self {
            parse_stack: ParseStack::new(),
        }
    }

    pub(in crate::spine) fn from_parse_stack(parse_stack: ParseStack) -> Self {
        Self { parse_stack }
    }

    pub(in crate::spine) fn restore_from_checkpoint(&mut self, checkpoint: &SpineCheckpoint) {
        self.parse_stack = checkpoint.parse_stack.clone();
    }

    #[cfg(test)]
    pub(in crate::spine) fn parse_stack(&self) -> &ParseStack {
        &self.parse_stack
    }

    pub(in crate::spine) fn parse_stack_with_memory_context_accounting(
        &self,
        accounting: &BTreeMap<String, i64>,
    ) -> ParseStack {
        let mut parse_stack = self.parse_stack.clone();
        parse_stack.apply_memory_context_accounting(accounting);
        parse_stack
    }

    #[cfg(test)]
    pub(in crate::spine) fn render_tree_with_memory_context_accounting(
        &self,
        accounting: &BTreeMap<String, i64>,
    ) -> Result<String, SpineError> {
        self.parse_stack_with_memory_context_accounting(accounting)
            .render_tree()
    }

    pub(in crate::spine) fn render_tree_with_context_annotations_and_memory_context_accounting(
        &self,
        annotations: &BTreeMap<NodeId, String>,
        accounting: &BTreeMap<String, i64>,
    ) -> Result<String, SpineError> {
        self.parse_stack_with_memory_context_accounting(accounting)
            .render_tree_with_context_annotations(annotations)
    }

    pub(in crate::spine) fn build_tree_snapshot_with_memory_context_accounting(
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

    pub(in crate::spine) fn current_open_meta_cloned(&self) -> Option<TreeMeta> {
        self.parse_stack.current_open_meta_opt().cloned()
    }

    #[cfg(test)]
    pub(in crate::spine) fn current_open_index(&self) -> Result<usize, SpineError> {
        Ok(self.parse_stack.current_open_meta()?.index)
    }

    #[cfg(test)]
    pub(in crate::spine) fn current_open_input_tokens(&self) -> Option<i64> {
        self.parse_stack
            .current_open_meta_opt()
            .and_then(|meta| meta.open_input_tokens)
    }

    pub(in crate::spine) fn current_close_open_meta(&self) -> Result<&TreeMeta, SpineError> {
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

    pub(in crate::spine) fn live_open_metas_cloned(&self) -> Vec<TreeMeta> {
        self.parse_stack
            .live_open_metas()
            .into_iter()
            .cloned()
            .collect()
    }

    pub(in crate::spine) fn last_visible_response_context_index(&self) -> Option<usize> {
        self.parse_stack.last_visible_response_context_index()
    }

    #[cfg(test)]
    pub(in crate::spine) fn visible_response_context_refs_for_test(&self) -> Vec<(u64, usize)> {
        self.parse_stack.visible_response_context_refs_for_test()
    }

    pub(in crate::spine) fn current_open_suffix_nodes_cloned(
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

    pub(in crate::spine) fn current_open_has_nodes(&self) -> Result<bool, SpineError> {
        self.parse_stack.current_open_has_nodes()
    }

    pub(in crate::spine) fn prepare_current_task_tree_reduction(
        &self,
        archive: &SpineArchive,
        memory: MemoryRef,
    ) -> Result<PreparedTaskTreeReduction, SpineError> {
        self.parse_stack
            .prepare_current_task_tree_reduction(archive, memory)
    }

    pub(in crate::spine) fn next_child_id(&self) -> Result<NodeId, SpineError> {
        self.parse_stack.next_child_id()
    }

    pub(in crate::spine) fn current_root_epoch_id(&self) -> Result<NodeId, SpineError> {
        self.parse_stack.current_root_epoch_id()
    }

    pub(in crate::spine) fn close_family_publication_plan(
        &self,
        operation: &'static str,
        suffix_start: usize,
        replacement_prefix: Vec<ResponseItem>,
        preserve_host_history_from: usize,
        atomic_mutable_context_segments: impl IntoIterator<
            Item = ParserPublicationToolcallSegmentEvidence,
        >,
    ) -> ParserPublicationPlan {
        close_family_publication_plan(
            operation,
            suffix_start,
            replacement_prefix,
            preserve_host_history_from,
            atomic_mutable_context_segments,
        )
    }

    pub(in crate::spine) fn root_compact_next_open_index_or_probe(
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
        // span, while next_open_index is the post-compact h(PS) variable context len.
        let probe_batch = crate::spine::lexer::plan_root_compact().lex_compact_batch(
            memory.clone(),
            0,
            next_open_input_tokens,
            next_open_context_tokens,
        )?;
        let probe_state = self.stage_lexed_batches(std::iter::once(&probe_batch), archive)?;
        root_compact_probe_variable_context_len(
            probe_state.parse_stack(),
            raw_items,
            staged_memory_body,
            trim_projection,
        )
    }

    #[cfg(test)]
    pub(in crate::spine) fn parse_stack_mut_for_test(&mut self) -> &mut ParseStack {
        &mut self.parse_stack
    }

    pub(in crate::spine) fn set_live_open_context_baseline(
        &mut self,
        node: &NodeId,
        input_tokens: i64,
        source: ContextBaselineSource,
    ) -> Result<bool, SpineError> {
        self.parse_stack
            .set_live_open_context_baseline(node, input_tokens, source)
    }

    pub(in crate::spine::parser) fn install_prepared_state(&mut self, state: ParserPreparedState) {
        self.parse_stack = state.into_parse_stack_for_install();
    }

    pub(in crate::spine) fn install_pending_close_after_side_effect_failure(
        &mut self,
        install: &ParserCommitPreparedInstall,
    ) {
        self.install_prepared_state(install.pending_state().clone());
    }

    pub(in crate::spine) fn install_prepared_commit(
        &mut self,
        install: ParserCommitPreparedInstall,
    ) {
        self.install_prepared_state(install.into_final_state());
    }

    pub(in crate::spine) fn install_prepared_commit_final(&mut self, install: ParserCommitInstall) {
        self.install_prepared_state(install.into_final_state());
    }

    pub(in crate::spine) fn install_prepared_observe(&mut self, install: ParserObserveInstall) {
        self.install_prepared_state(install.into_final_state());
    }

    pub(in crate::spine) fn install_prepared_open(&mut self, install: ParserOpenInstall) {
        self.install_prepared_state(install.into_final_state());
    }

    pub(in crate::spine) fn install_pending_root_compact_after_side_effect_failure(
        &mut self,
        install: &ParserRootCompactPreparedCommitInstall,
    ) {
        self.install_prepared_state(install.pending_state().clone());
    }

    pub(in crate::spine) fn install_prepared_root_compact(
        &mut self,
        install: ParserRootCompactPreparedCommitInstall,
    ) {
        self.install_prepared_state(install.into_final_state());
    }

    pub(in crate::spine::parser) fn stage_lexed_batches<'a>(
        &self,
        batches: impl IntoIterator<Item = &'a LexedTokenBatch>,
        archive: &SpineArchive,
    ) -> Result<ParserPreparedState, SpineError> {
        let mut staged = self.parse_stack.clone();
        apply_lexed_batches_to_parse_stack(&mut staged, batches, archive)?;
        Ok(ParserPreparedState::new(staged))
    }

    pub(in crate::spine) fn prepare_open_install(
        &self,
        open_lexed: &LexedTokenBatch,
        toolcall_lexed: Option<&LexedTokenBatch>,
        archive: &SpineArchive,
    ) -> Result<ParserOpenInstall, SpineError> {
        let batches = std::iter::once(open_lexed).chain(toolcall_lexed);
        let staged = self.stage_lexed_batches(batches, archive)?;
        Ok(ParserOpenInstall::new(staged))
    }

    pub(in crate::spine) fn close_reduced_next_child_id(
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

    pub(in crate::spine) fn prepare_close_family_install(
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

    pub(in crate::spine) fn prepare_root_compact_txn(
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
        let root_compact_publication = root_compact_publication_from_state(
            &final_parse_stack,
            raw_items,
            staged_memory_body,
            trim_projection,
        )?;
        Ok(ParserRootCompactPreparedTxn::new(
            root_compact_publication,
            ParserRootCompactPreparedInstall::new(
                ParserRootCompactPendingInstall::new(ParserPreparedState::new(pending)),
                ParserRootCompactInstall::new(ParserPreparedState::new(final_parse_stack)),
            ),
        ))
    }

    pub(in crate::spine) fn consume_lexed_batch(
        &self,
        lexed: &LexedTokenBatch,
        archive: &SpineArchive,
    ) -> Result<ParserObserveInstall, SpineError> {
        let staged = self.stage_lexed_batches(std::iter::once(lexed), archive)?;
        Ok(ParserObserveInstall::new(staged))
    }

    pub(in crate::spine) fn materialize_variable_context(
        &self,
        raw_items: &[Option<ResponseItem>],
        trim_projection: &TrimProjection,
    ) -> Result<Vec<ResponseItem>, SpineError> {
        materialize_variable_context_from_state(&self.parse_stack, raw_items, trim_projection)
    }

    pub(in crate::spine) fn variable_context_len(
        &self,
        raw_items: &[Option<ResponseItem>],
        trim_projection: &TrimProjection,
    ) -> Result<usize, SpineError> {
        Ok(self
            .materialize_variable_context(raw_items, trim_projection)?
            .len())
    }

    pub(in crate::spine) fn build_checkpoint(
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
        let proof = checkpoint_publication_proof_from_parse_stack(
            &self.parse_stack,
            raw_items,
            trim_projection,
        )?;
        build_checkpoint(
            rollout_path,
            raw_ordinal,
            token_seq,
            pressure_seq_watermark,
            trim_seq_watermark,
            raw_live,
            proof.parse_stack(),
            proof.variable_context(),
        )
    }

    pub(in crate::spine) fn validate_checkpoint_parse_stack(
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
    pub(in crate::spine) fn msg_leaf_count_for_test(&self) -> usize {
        parse_stack_msg_leaf_count(&self.parse_stack.symbols)
    }

    #[cfg(test)]
    pub(in crate::spine) fn toolcall_leaf_count_for_test(&self) -> usize {
        parse_stack_toolcall_leaf_count(&self.parse_stack.symbols)
    }

    #[cfg(test)]
    pub(in crate::spine) fn debug_for_test(&self) -> String {
        format!("{:?}", self.parse_stack)
    }
}
