use crate::spine::SpineError;
use crate::spine::archive::SpineArchive;
use crate::spine::archive::archive_task_tree;
use crate::spine::archive::next_root_open_symbol;
use crate::spine::model::ControlSymbol;
use crate::spine::model::MemoryRef;
use crate::spine::model::NodeId;
use crate::spine::model::RootEpoch;
use crate::spine::model::SpineToken;
use crate::spine::model::SpineTreeNode;
use crate::spine::model::Symbol;
use crate::spine::model::TreeMeta;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

mod accounting;
mod replay;
mod tree;

pub(in crate::spine) use replay::apply_metadata_event;
pub(in crate::spine) use replay::event_to_token;
#[cfg(test)]
pub(super) use tree::parse_stack_msg_leaf_count;
#[cfg(test)]
pub(super) use tree::parse_stack_toolcall_leaf_count;

#[derive(Clone, Debug)]
pub(super) struct PreparedTaskTreeReduction {
    meta: TreeMeta,
    children: Vec<SpineTreeNode>,
    memory: MemoryRef,
    memory_path: PathBuf,
    trajs_path: PathBuf,
}

#[derive(Clone, Debug)]
pub(super) struct PreparedRootEpochReduction {
    compact_idx: usize,
    boundary_idx: usize,
    boundary: Symbol,
    root_epoch: RootEpoch,
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
        self.symbols.push(symbol);
        self.reduce_fixpoint(archive)
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
        while self.reduce_task_tree(archive)?
            || self.reduce_root_epoch(archive)?
            || self.reduce_nodes_append()
            || self.reduce_node_to_nodes()
        {}
        Ok(())
    }

    fn reduce_task_tree(&mut self, archive: &SpineArchive) -> Result<bool, SpineError> {
        match self.symbols.get(..) {
            Some(
                [
                    ..,
                    Symbol::Control(ControlSymbol::Open(_)),
                    Symbol::Control(ControlSymbol::Close(_)),
                ],
            ) => {
                return Err(SpineError::InvalidEvent(
                    "spine.close requires non-empty live suffix".to_string(),
                ));
            }
            Some(
                [
                    ..,
                    Symbol::Control(ControlSymbol::Open(_)),
                    Symbol::SpineTreeNodes(_),
                    Symbol::Control(ControlSymbol::Close(_)),
                ],
            ) => {}
            _ => return Ok(false),
        }
        let len = self.symbols.len();
        let (
            Symbol::Control(ControlSymbol::Open(meta)),
            Symbol::SpineTreeNodes(children),
            Symbol::Control(ControlSymbol::Close(memory)),
        ) = (
            self.symbols[len - 3].clone(),
            self.symbols[len - 2].clone(),
            self.symbols[len - 1].clone(),
        )
        else {
            unreachable!("close reduction suffix was checked before clone")
        };
        let (memory_path, trajs_path) = archive_task_tree(archive, &meta, &children, &memory)?;
        self.symbols.truncate(len - 3);
        self.symbols
            .push(Symbol::SpineTreeNode(SpineTreeNode::SpineTree {
                memory,
                meta,
                children,
                memory_path,
                trajs_path,
            }));
        Ok(true)
    }

    pub(super) fn prepare_current_task_tree_reduction(
        &self,
        archive: &SpineArchive,
        memory: MemoryRef,
    ) -> Result<PreparedTaskTreeReduction, SpineError> {
        let len = self.symbols.len();
        let Some((meta, children)) = self.current_task_tree_suffix() else {
            return Err(SpineError::InvalidEvent(
                "spine.close requires a live task tree suffix".to_string(),
            ));
        };
        if self
            .symbols
            .get(len.saturating_sub(1))
            .is_some_and(|symbol| matches!(symbol, Symbol::Control(ControlSymbol::Close(_))))
        {
            let Symbol::Control(ControlSymbol::Close(existing)) = &self.symbols[len - 1] else {
                unreachable!("close suffix checked before match")
            };
            if existing != &memory {
                return Err(SpineError::InvalidEvent(format!(
                    "pending spine.close memory {} does not match prepared memory {}",
                    existing.compact_id, memory.compact_id
                )));
            }
        }
        let (memory_path, trajs_path) = archive_task_tree(archive, meta, children, &memory)?;
        Ok(PreparedTaskTreeReduction {
            meta: meta.clone(),
            children: children.to_vec(),
            memory,
            memory_path,
            trajs_path,
        })
    }

    pub(super) fn shift_pending_close(
        &mut self,
        memory: MemoryRef,
        archive: &SpineArchive,
    ) -> Result<(), SpineError> {
        if self.pending_close_memory()?.is_some() {
            self.validate_pending_task_tree_reduction_memory(&memory)?;
            return Ok(());
        }
        self.reduce_fixpoint(archive)?;
        if self.current_task_tree_suffix().is_none() {
            return Err(SpineError::InvalidEvent(
                "spine.close requires a live task tree suffix".to_string(),
            ));
        }
        self.symbols
            .push(Symbol::Control(ControlSymbol::Close(memory)));
        Ok(())
    }

    pub(super) fn validate_pending_task_tree_reduction(
        &self,
        reduction: &PreparedTaskTreeReduction,
    ) -> Result<(), SpineError> {
        let len = self.symbols.len();
        let Some(
            [
                ..,
                Symbol::Control(ControlSymbol::Open(meta)),
                Symbol::SpineTreeNodes(children),
                Symbol::Control(ControlSymbol::Close(memory)),
            ],
        ) = self.symbols.get(..)
        else {
            return Err(SpineError::InvalidEvent(
                "spine.close reduction requires a pending Close suffix".to_string(),
            ));
        };
        if len < 3 {
            return Err(SpineError::InvalidEvent(
                "spine.close reduction suffix underflow".to_string(),
            ));
        }
        if meta != &reduction.meta || children != &reduction.children || memory != &reduction.memory
        {
            return Err(SpineError::InvalidEvent(
                "pending spine.close suffix changed before reduction".to_string(),
            ));
        }
        Ok(())
    }

    pub(super) fn apply_prevalidated_task_tree_reduction(
        &mut self,
        reduction: PreparedTaskTreeReduction,
    ) {
        debug_assert!(
            self.validate_pending_task_tree_reduction(&reduction)
                .is_ok()
        );
        let len = self.symbols.len();
        self.symbols.truncate(len - 3);
        self.symbols
            .push(Symbol::SpineTreeNode(SpineTreeNode::SpineTree {
                memory: reduction.memory,
                meta: reduction.meta,
                children: reduction.children,
                memory_path: reduction.memory_path,
                trajs_path: reduction.trajs_path,
            }));
        self.reduce_nodes_fixpoint();
    }

    pub(super) fn task_tree_reduced(
        &self,
        reduction: PreparedTaskTreeReduction,
    ) -> Result<Self, SpineError> {
        self.validate_pending_task_tree_reduction(&reduction)?;
        let mut reduced = self.clone();
        reduced.apply_prevalidated_task_tree_reduction(reduction);
        Ok(reduced)
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

    fn reduce_root_epoch(&mut self, archive: &SpineArchive) -> Result<bool, SpineError> {
        let Some(compact_idx) = self
            .symbols
            .iter()
            .rposition(|symbol| matches!(symbol, Symbol::Control(ControlSymbol::Compact(..))))
        else {
            return Ok(false);
        };
        let Symbol::Control(ControlSymbol::Compact(
            memory,
            next_open_index,
            next_open_input_tokens,
            next_open_context_tokens,
        )) = self.symbols[compact_idx].clone()
        else {
            unreachable!("compact symbol was checked before clone")
        };
        let next_open = next_root_open_symbol(
            archive,
            &memory,
            next_open_index,
            next_open_input_tokens,
            next_open_context_tokens,
        )?;
        let Some(boundary_idx) = self.symbols[..compact_idx].iter().rposition(|symbol| {
            matches!(
                symbol,
                Symbol::Control(ControlSymbol::Init(_)) | Symbol::RootEpoches(_)
            )
        }) else {
            return Ok(false);
        };

        let root_epoch = RootEpoch { memory };
        let boundary = self.symbols[boundary_idx].clone();
        self.apply_root_epoch_boundary(boundary_idx, boundary, root_epoch);
        self.symbols.push(next_open);
        Ok(true)
    }

    pub(super) fn prepare_root_epoch_reduction(
        &self,
        archive: &SpineArchive,
        memory: MemoryRef,
        next_open_index: usize,
        next_open_input_tokens: Option<i64>,
        next_open_context_tokens: Option<i64>,
    ) -> Result<PreparedRootEpochReduction, SpineError> {
        let next_open = next_root_open_symbol(
            archive,
            &memory,
            next_open_index,
            next_open_input_tokens,
            next_open_context_tokens,
        )?;
        let Some(boundary_idx) = self.symbols.iter().rposition(|symbol| {
            matches!(
                symbol,
                Symbol::Control(ControlSymbol::Init(_)) | Symbol::RootEpoches(_)
            )
        }) else {
            return Err(SpineError::InvalidEvent(
                "root compact has no root epoch boundary".to_string(),
            ));
        };
        let compact_idx = if self
            .pending_compact_next_open_index(
                &memory,
                next_open_input_tokens,
                next_open_context_tokens,
            )?
            .is_some()
        {
            self.symbols.len() - 1
        } else {
            self.symbols.len()
        };
        Ok(PreparedRootEpochReduction {
            compact_idx,
            boundary_idx,
            boundary: self.symbols[boundary_idx].clone(),
            root_epoch: RootEpoch { memory },
            next_open,
        })
    }

    pub(super) fn shift_pending_compact(
        &mut self,
        memory: MemoryRef,
        next_open_index: usize,
        next_open_input_tokens: Option<i64>,
        next_open_context_tokens: Option<i64>,
        archive: &SpineArchive,
    ) -> Result<(), SpineError> {
        if self.pending_compact_memory().is_some() {
            self.validate_pending_compact_memory(&memory)?;
            return Ok(());
        }
        self.reduce_fixpoint(archive)?;
        self.symbols.push(Symbol::Control(ControlSymbol::Compact(
            memory,
            next_open_index,
            next_open_input_tokens,
            next_open_context_tokens,
        )));
        Ok(())
    }

    pub(super) fn validate_pending_root_epoch_reduction(
        &self,
        reduction: &PreparedRootEpochReduction,
    ) -> Result<(), SpineError> {
        let Some(Symbol::Control(ControlSymbol::Compact(
            memory,
            next_open_index,
            next_open_input_tokens,
            next_open_context_tokens,
        ))) = self.symbols.get(reduction.compact_idx)
        else {
            return Err(SpineError::InvalidEvent(
                "root compact reduction requires a pending Compact token".to_string(),
            ));
        };
        let Symbol::Control(ControlSymbol::Open(next_open)) = &reduction.next_open else {
            return Err(SpineError::Invariant(
                "root compact prepared next open is not an Open symbol".to_string(),
            ));
        };
        if &reduction.root_epoch.memory != memory
            || next_open.index != *next_open_index
            || next_open.open_input_tokens != *next_open_input_tokens
            || next_open.open_context_tokens != *next_open_context_tokens
        {
            return Err(SpineError::InvalidEvent(
                "pending root compact token changed before reduction".to_string(),
            ));
        }
        Ok(())
    }

    pub(super) fn apply_prevalidated_root_epoch_reduction(
        &mut self,
        reduction: PreparedRootEpochReduction,
    ) {
        debug_assert!(
            self.validate_pending_root_epoch_reduction(&reduction)
                .is_ok()
        );
        self.apply_root_epoch_boundary(
            reduction.boundary_idx,
            reduction.boundary,
            reduction.root_epoch,
        );
        self.symbols.push(reduction.next_open);
    }

    fn apply_root_epoch_boundary(
        &mut self,
        boundary_idx: usize,
        boundary: Symbol,
        root_epoch: RootEpoch,
    ) {
        match boundary {
            Symbol::Control(ControlSymbol::Init(_)) => {
                self.symbols.truncate(boundary_idx + 1);
                self.symbols.push(Symbol::RootEpoches(vec![root_epoch]));
            }
            Symbol::RootEpoches(mut root_epochs) => {
                self.symbols.truncate(boundary_idx);
                root_epochs.push(root_epoch);
                self.symbols.push(Symbol::RootEpoches(root_epochs));
            }
            _ => unreachable!("root epoch boundary was checked before apply"),
        }
    }

    pub(super) fn root_epoch_reduced(
        &self,
        reduction: PreparedRootEpochReduction,
    ) -> Result<Self, SpineError> {
        self.validate_pending_root_epoch_reduction(&reduction)?;
        let mut reduced = self.clone();
        reduced.apply_prevalidated_root_epoch_reduction(reduction);
        Ok(reduced)
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

    pub(super) fn current_open_meta(&self) -> Result<&TreeMeta, SpineError> {
        self.current_open_meta_opt()
            .ok_or_else(|| SpineError::InvalidEvent("ParseStack has no live Open".to_string()))
    }

    pub(super) fn current_open_meta_opt(&self) -> Option<&TreeMeta> {
        self.symbols.iter().rev().find_map(|symbol| match symbol {
            Symbol::Control(ControlSymbol::Open(meta)) => Some(meta),
            _ => None,
        })
    }

    pub(super) fn live_open_metas(&self) -> Vec<&TreeMeta> {
        self.symbols
            .iter()
            .filter_map(|symbol| match symbol {
                Symbol::Control(ControlSymbol::Open(meta)) => Some(meta),
                _ => None,
            })
            .collect()
    }

    pub(super) fn set_live_open_context_baseline(
        &mut self,
        node: &NodeId,
        provider_input_tokens: i64,
        source: crate::spine::model::ContextBaselineSource,
    ) -> Result<bool, SpineError> {
        let Some(meta) = self
            .symbols
            .iter_mut()
            .rev()
            .find_map(|symbol| match symbol {
                Symbol::Control(ControlSymbol::Open(meta)) if &meta.id == node => Some(meta),
                _ => None,
            })
        else {
            return Ok(false);
        };
        match (
            meta.open_input_tokens,
            meta.open_context_tokens,
            meta.open_context_source,
        ) {
            (None, None, None) => {
                meta.open_input_tokens = Some(provider_input_tokens);
                meta.open_context_tokens = Some(provider_input_tokens);
                meta.open_context_source = Some(source);
                Ok(true)
            }
            (Some(existing_input), Some(existing_context), Some(existing_source))
                if existing_input == provider_input_tokens
                    && existing_context == provider_input_tokens
                    && existing_source == source =>
            {
                Ok(false)
            }
            _ => Err(SpineError::InvalidEvent(format!(
                "open context baseline for node {node} is already set"
            ))),
        }
    }

    pub(super) fn current_open_has_nodes(&self) -> Result<bool, SpineError> {
        let open_idx = self
            .symbols
            .iter()
            .rposition(|symbol| matches!(symbol, Symbol::Control(ControlSymbol::Open(_))))
            .ok_or_else(|| SpineError::InvalidEvent("ParseStack has no live Open".to_string()))?;
        Ok(self.symbols[open_idx + 1..]
            .iter()
            .any(|symbol| matches!(symbol, Symbol::SpineTreeNodes(nodes) if !nodes.is_empty())))
    }

    fn current_task_tree_suffix(&self) -> Option<(&TreeMeta, &[SpineTreeNode])> {
        match self.symbols.get(..) {
            Some(
                [
                    ..,
                    Symbol::Control(ControlSymbol::Open(meta)),
                    Symbol::SpineTreeNodes(children),
                ]
                | [
                    ..,
                    Symbol::Control(ControlSymbol::Open(meta)),
                    Symbol::SpineTreeNodes(children),
                    Symbol::Control(ControlSymbol::Close(_)),
                ],
            ) if !children.is_empty() => Some((meta, children)),
            _ => None,
        }
    }

    fn pending_close_memory(&self) -> Result<Option<&MemoryRef>, SpineError> {
        match self.symbols.get(..) {
            Some(
                [
                    ..,
                    Symbol::Control(ControlSymbol::Open(_)),
                    Symbol::SpineTreeNodes(_),
                    Symbol::Control(ControlSymbol::Close(memory)),
                ],
            ) => Ok(Some(memory)),
            Some(
                [
                    ..,
                    Symbol::Control(ControlSymbol::Open(_)),
                    Symbol::Control(ControlSymbol::Close(_)),
                ],
            ) => Err(SpineError::InvalidEvent(
                "spine.close requires non-empty live suffix".to_string(),
            )),
            _ => Ok(None),
        }
    }

    fn validate_pending_task_tree_reduction_memory(
        &self,
        memory: &MemoryRef,
    ) -> Result<(), SpineError> {
        let Some(existing) = self.pending_close_memory()? else {
            return Ok(());
        };
        if existing != memory {
            return Err(SpineError::InvalidEvent(format!(
                "pending spine.close memory {} does not match prepared memory {}",
                existing.compact_id, memory.compact_id
            )));
        }
        Ok(())
    }

    fn pending_compact_memory(&self) -> Option<&MemoryRef> {
        match self.symbols.last() {
            Some(Symbol::Control(ControlSymbol::Compact(memory, ..))) => Some(memory),
            _ => None,
        }
    }

    fn validate_pending_compact_memory(&self, memory: &MemoryRef) -> Result<(), SpineError> {
        let Some(existing) = self.pending_compact_memory() else {
            return Ok(());
        };
        if existing != memory {
            return Err(SpineError::InvalidEvent(format!(
                "pending root compact memory {} does not match prepared memory {}",
                existing.compact_id, memory.compact_id
            )));
        }
        Ok(())
    }

    pub(super) fn pending_compact_next_open_index(
        &self,
        memory: &MemoryRef,
        next_open_input_tokens: Option<i64>,
        next_open_context_tokens: Option<i64>,
    ) -> Result<Option<usize>, SpineError> {
        let Some(Symbol::Control(ControlSymbol::Compact(
            existing,
            next_open_index,
            existing_input_tokens,
            existing_context_tokens,
        ))) = self.symbols.last()
        else {
            return Ok(None);
        };
        if existing != memory
            || *existing_input_tokens != next_open_input_tokens
            || *existing_context_tokens != next_open_context_tokens
        {
            return Err(SpineError::InvalidEvent(format!(
                "pending root compact memory {} does not match prepared memory {}",
                existing.compact_id, memory.compact_id
            )));
        }
        Ok(Some(*next_open_index))
    }

    pub(super) fn current_root_epoch_id(&self) -> Result<NodeId, SpineError> {
        let current = self.current_cursor_id()?;
        let root = *current
            .0
            .first()
            .ok_or_else(|| SpineError::InvalidEvent("current node id is empty".to_string()))?;
        Ok(NodeId::root_epoch(root))
    }

    pub(super) fn current_cursor_id(&self) -> Result<NodeId, SpineError> {
        if let Some(open) = self.current_open_meta_opt() {
            return Ok(open.id.clone());
        }
        for symbol in self.symbols.iter().rev() {
            match symbol {
                Symbol::SpineTreeNodes(nodes) => {
                    if let Some(root) = tree::root_epoch_from_nodes(nodes) {
                        return Ok(root);
                    }
                }
                Symbol::SpineTreeNode(node) => {
                    if let Some(root) = tree::root_epoch_from_node(node) {
                        return Ok(root);
                    }
                }
                Symbol::RootEpoches(root_epochs) => {
                    let next = root_epochs
                        .last()
                        .and_then(|root_epoch| root_epoch.memory.node_id.0.first().copied())
                        .and_then(|root| root.checked_add(1))
                        .ok_or_else(|| {
                            SpineError::InvalidEvent(
                                "current root epoch id is unavailable".to_string(),
                            )
                        })?;
                    return Ok(NodeId::root_epoch(next));
                }
                Symbol::Control(ControlSymbol::Init(meta)) => return Ok(meta.id.clone()),
                Symbol::Control(_) => {}
            }
        }
        Err(SpineError::InvalidEvent(
            "ParseStack has no cursor".to_string(),
        ))
    }

    pub(super) fn next_child_id(&self) -> Result<NodeId, SpineError> {
        tree::next_child_id(self)
    }
}
