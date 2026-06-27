use crate::spine::SpineError;
use crate::spine::archive::SpineArchive;
use crate::spine::model::ControlSymbol;
use crate::spine::model::MemoryRef;
use crate::spine::model::NodeId;
use crate::spine::model::SpineToken;
use crate::spine::model::SpineTreeNode;
use crate::spine::model::Symbol;
use crate::spine::model::TreeMeta;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

mod accounting;
mod context;
mod cursor;
mod root_epoch;
mod task_tree;
mod tree;

#[cfg(test)]
pub(super) use tree::parse_stack_msg_leaf_count;
#[cfg(test)]
pub(super) use tree::parse_stack_toolcall_leaf_count;

#[derive(Clone, Debug)]
pub(super) struct PreparedTaskTreeReduction {
    pub(super) meta: TreeMeta,
    pub(super) children: Vec<SpineTreeNode>,
    pub(super) memory: MemoryRef,
    pub(super) memory_path: PathBuf,
    pub(super) trajs_path: PathBuf,
}

#[derive(Clone, Debug)]
pub(super) struct PreparedRootEpochReduction {
    compact_idx: usize,
    boundary_idx: usize,
    boundary: Symbol,
    root_epoch: crate::spine::model::RootEpoch,
    next_open: Symbol,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct ParseStack {
    pub(super) symbols: Vec<Symbol>,
}

impl ParseStack {
    pub(super) fn new() -> Self {
        Self {
            symbols: Vec::new(),
        }
    }

    pub(super) fn shift(
        &mut self,
        token: SpineToken,
        archive: &SpineArchive,
    ) -> Result<(), SpineError> {
        self.reduce_fixpoint(archive)?;
        let previous_visible_context_index = self.last_visible_response_context_index();
        let symbol = match token {
            SpineToken::Init { meta } => Symbol::Control(ControlSymbol::Init(meta)),
            SpineToken::End => Symbol::Control(ControlSymbol::End),
            SpineToken::Open { meta } => Symbol::Control(ControlSymbol::Open(meta)),
            SpineToken::Close { memory } => Symbol::Control(ControlSymbol::Close(memory)),
            SpineToken::Compact {
                memory,
                next_open_index,
                next_open_input_tokens,
                next_open_context_tokens,
            } => Symbol::Control(ControlSymbol::Compact(
                memory,
                next_open_index,
                next_open_input_tokens,
                next_open_context_tokens,
            )),
            SpineToken::Msg {
                seg,
                from_user,
                user_anchor,
            } => Symbol::SpineTreeNode(SpineTreeNode::MsgAsLeafNode {
                msg: seg,
                from_user,
                user_anchor,
            }),
            SpineToken::ToolCall { segments } => {
                Symbol::SpineTreeNode(SpineTreeNode::ToolCallAsLeafNode { segments })
            }
        };
        context::validate_shifted_symbol_context_indices(previous_visible_context_index, &symbol)?;
        self.symbols.push(symbol);
        self.reduce_fixpoint(archive)
    }

    pub(super) fn last_visible_response_context_index(&self) -> Option<usize> {
        self.symbols
            .iter()
            .flat_map(context::symbol_response_context_refs)
            .map(|(_, context_index)| context_index)
            .max()
    }

    #[cfg(test)]
    pub(super) fn visible_response_context_refs_for_test(&self) -> Vec<(u64, usize)> {
        self.symbols
            .iter()
            .flat_map(context::symbol_response_context_refs)
            .collect()
    }

    pub(super) fn apply_memory_context_accounting(&mut self, accounting: &BTreeMap<String, i64>) {
        if accounting.is_empty() {
            return;
        }
        for symbol in &mut self.symbols {
            accounting::apply_memory_context_accounting_to_symbol(symbol, accounting);
        }
    }

    fn reduce_fixpoint(&mut self, archive: &SpineArchive) -> Result<(), SpineError> {
        while task_tree::reduce_task_tree(self, archive)?
            || root_epoch::reduce_root_epoch(self, archive)?
            || self.reduce_nodes_append()
            || self.reduce_node_to_nodes()
        {}
        Ok(())
    }

    pub(super) fn prepare_current_task_tree_reduction(
        &self,
        archive: &SpineArchive,
        memory: MemoryRef,
    ) -> Result<PreparedTaskTreeReduction, SpineError> {
        task_tree::prepare_current_task_tree_reduction(self, archive, memory)
    }

    pub(super) fn shift_pending_close(
        &mut self,
        memory: MemoryRef,
        archive: &SpineArchive,
    ) -> Result<(), SpineError> {
        task_tree::shift_pending_close(self, memory, archive)
    }

    pub(super) fn validate_pending_task_tree_reduction(
        &self,
        reduction: &PreparedTaskTreeReduction,
    ) -> Result<(), SpineError> {
        task_tree::validate_pending_task_tree_reduction(self, reduction)
    }

    pub(super) fn apply_prevalidated_task_tree_reduction(
        &mut self,
        reduction: PreparedTaskTreeReduction,
    ) {
        task_tree::apply_prevalidated_task_tree_reduction(self, reduction);
    }

    pub(super) fn task_tree_reduced(
        &self,
        reduction: PreparedTaskTreeReduction,
    ) -> Result<Self, SpineError> {
        task_tree::task_tree_reduced(self, reduction)
    }

    fn reduce_nodes_append(&mut self) -> bool {
        let Some([.., Symbol::SpineTreeNodes(_), Symbol::SpineTreeNode(_)]) = self.symbols.get(..)
        else {
            return false;
        };
        let node = self
            .symbols
            .pop()
            .expect("node symbol matched by reduce pattern");
        let Some(Symbol::SpineTreeNodes(nodes)) = self.symbols.last_mut() else {
            unreachable!("nodes symbol was checked before pop")
        };
        let Symbol::SpineTreeNode(node) = node else {
            unreachable!("node symbol was checked before pop")
        };
        nodes.push(node);
        true
    }

    fn reduce_node_to_nodes(&mut self) -> bool {
        let Some(Symbol::SpineTreeNode(_)) = self.symbols.last() else {
            return false;
        };
        let Some(Symbol::SpineTreeNode(node)) = self.symbols.pop() else {
            unreachable!("node symbol was checked before pop")
        };
        self.symbols.push(Symbol::SpineTreeNodes(vec![node]));
        true
    }

    fn reduce_nodes_fixpoint(&mut self) {
        while self.reduce_nodes_append() || self.reduce_node_to_nodes() {}
    }

    pub(super) fn prepare_root_epoch_reduction(
        &self,
        archive: &SpineArchive,
        memory: MemoryRef,
        next_open_index: usize,
        next_open_input_tokens: Option<i64>,
        next_open_context_tokens: Option<i64>,
    ) -> Result<PreparedRootEpochReduction, SpineError> {
        root_epoch::prepare_root_epoch_reduction(
            self,
            archive,
            memory,
            next_open_index,
            next_open_input_tokens,
            next_open_context_tokens,
        )
    }

    pub(super) fn shift_pending_compact(
        &mut self,
        memory: MemoryRef,
        next_open_index: usize,
        next_open_input_tokens: Option<i64>,
        next_open_context_tokens: Option<i64>,
        archive: &SpineArchive,
    ) -> Result<(), SpineError> {
        root_epoch::shift_pending_compact(
            self,
            memory,
            next_open_index,
            next_open_input_tokens,
            next_open_context_tokens,
            archive,
        )
    }

    pub(super) fn validate_pending_root_epoch_reduction(
        &self,
        reduction: &PreparedRootEpochReduction,
    ) -> Result<(), SpineError> {
        root_epoch::validate_pending_root_epoch_reduction(self, reduction)
    }

    pub(super) fn apply_prevalidated_root_epoch_reduction(
        &mut self,
        reduction: PreparedRootEpochReduction,
    ) {
        root_epoch::apply_prevalidated_root_epoch_reduction(self, reduction);
    }

    pub(super) fn root_epoch_reduced(
        &self,
        reduction: PreparedRootEpochReduction,
    ) -> Result<Self, SpineError> {
        root_epoch::root_epoch_reduced(self, reduction)
    }

    #[cfg(test)]
    pub(super) fn render_tree(&self) -> Result<String, SpineError> {
        tree::render_tree(self)
    }

    pub(super) fn render_tree_with_context_annotations(
        &self,
        annotations: &BTreeMap<NodeId, String>,
    ) -> Result<String, SpineError> {
        tree::render_tree_with_context_annotations(self, annotations)
    }

    pub(super) fn tree_snapshot_nodes(
        &self,
    ) -> Result<Vec<codex_protocol::spine_tree::SpineTreeNodeSnapshot>, SpineError> {
        tree::tree_snapshot_nodes(self)
    }

    pub(super) fn pending_compact_next_open_index(
        &self,
        memory: &MemoryRef,
        next_open_input_tokens: Option<i64>,
        next_open_context_tokens: Option<i64>,
    ) -> Result<Option<usize>, SpineError> {
        root_epoch::pending_compact_next_open_index(
            self,
            memory,
            next_open_input_tokens,
            next_open_context_tokens,
        )
    }
}
