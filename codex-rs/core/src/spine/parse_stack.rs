use crate::spine::SpineError;
use crate::spine::archive::SpineArchive;
use crate::spine::archive::archive_task_tree;
use crate::spine::archive::memory_ref;
use crate::spine::archive::next_root_open_symbol;
use crate::spine::archive::tree_meta;
use crate::spine::archive::tree_meta_with_token_baselines;
use crate::spine::model::ControlSymbol;
use crate::spine::model::KEvent;
use crate::spine::model::LoggedKEvent;
use crate::spine::model::MemRecord;
use crate::spine::model::MemoryRef;
use crate::spine::model::NodeId;
use crate::spine::model::NodeStatus;
use crate::spine::model::RawMask;
use crate::spine::model::RootEpoch;
use crate::spine::model::SegRef;
use crate::spine::model::SpineToken;
use crate::spine::model::SpineTreeNode;
use crate::spine::model::Symbol;
use crate::spine::model::TreeMeta;
use codex_protocol::num_format::format_si_suffix;
use codex_protocol::spine_tree::SpineTreeNodeAccountingSnapshot;
use codex_protocol::spine_tree::SpineTreeNodeSnapshot;
use codex_protocol::spine_tree::SpineTreeNodeStatus;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::PathBuf;

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
            SpineToken::Msg { seg, from_user } => {
                Symbol::SpineTreeNode(SpineTreeNode::MsgAsLeafNode {
                    msg: seg,
                    from_user,
                })
            }
        };
        self.symbols.push(symbol);
        self.reduce_fixpoint(archive)
    }

    fn reduce_fixpoint(&mut self, archive: &SpineArchive) -> Result<(), SpineError> {
        loop {
            if self.reduce_task_tree(archive)? {
                continue;
            }
            if self.reduce_root_epoch(archive)? {
                continue;
            }
            if self.reduce_nodes_append() {
                continue;
            }
            if self.reduce_node_to_nodes() {
                continue;
            }
            break;
        }
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
        let Some(Symbol::Control(ControlSymbol::Close(memory))) = self.symbols.pop() else {
            unreachable!("close symbol matched by reduce pattern")
        };
        let Some(Symbol::SpineTreeNodes(children)) = self.symbols.pop() else {
            unreachable!("nodes symbol matched by reduce pattern")
        };
        let Some(Symbol::Control(ControlSymbol::Open(meta))) = self.symbols.pop() else {
            unreachable!("open symbol matched by reduce pattern")
        };
        let (memory_path, trajs_path) = archive_task_tree(archive, &meta, &children, &memory)?;
        self.symbols
            .push(Symbol::SpineTreeNode(SpineTreeNode::SpineTree {
                memory: memory.clone(),
                meta,
                children,
                memory_path,
                trajs_path,
            }));
        Ok(true)
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
            _ => unreachable!("root epoch boundary was checked before mutate"),
        }
        self.symbols.push(next_open);
        Ok(true)
    }

    pub(super) fn render_tree(&self) -> Result<String, SpineError> {
        let rows = self.tree_rows()?;
        Ok(format_tree_rows(rows, None))
    }

    pub(super) fn render_tree_with_current_annotation(
        &self,
        current_annotation: Option<&str>,
    ) -> Result<String, SpineError> {
        let rows = self.tree_rows()?;
        Ok(format_tree_rows(rows, current_annotation))
    }

    pub(super) fn tree_snapshot_nodes(&self) -> Result<Vec<SpineTreeNodeSnapshot>, SpineError> {
        let rows = self.tree_rows()?;
        let projected_ids = rows
            .iter()
            .map(|row| row.id.clone())
            .collect::<BTreeSet<_>>();
        Ok(rows
            .into_iter()
            .map(|row| {
                let status = row.snapshot_status();
                let summary = visible_summary(&row).map(str::to_string);
                // Snapshot parents describe this projected forest, not hidden
                // ParseStack ancestors such as the root epoch holder.
                let parent_id = row
                    .id
                    .parent()
                    .filter(|parent| projected_ids.contains(parent))
                    .map(|id| id.as_path());
                SpineTreeNodeSnapshot {
                    parent_id,
                    node_id: row.id.as_path(),
                    summary,
                    status,
                    accounting: row.accounting.as_ref().and_then(snapshot_accounting),
                }
            })
            .collect())
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
                    if let Some(root) = root_epoch_from_nodes(nodes) {
                        return Ok(root);
                    }
                }
                Symbol::SpineTreeNode(node) => {
                    if let Some(root) = root_epoch_from_node(node) {
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
        let parent = self.current_cursor_id()?;
        let rows = self.tree_rows()?;
        let child_count = rows
            .iter()
            .filter(|row| row.id.parent().as_ref() == Some(&parent))
            .count();
        let index = u32::try_from(child_count + 1)
            .map_err(|_| SpineError::InvalidEvent("too many child nodes".to_string()))?;
        Ok(parent.child(index))
    }

    fn tree_rows(&self) -> Result<Vec<TreeRenderRow>, SpineError> {
        let mut rows = Vec::<TreeRenderRow>::new();
        collect_tree_render_rows(&self.symbols, &mut rows)?;
        project_current_root_epoch_row(self.current_cursor_id()?, &mut rows);
        mark_cursor_statuses(self.current_cursor_id()?, &mut rows);
        Ok(rows)
    }
}

impl From<NodeStatus> for SpineTreeNodeStatus {
    fn from(value: NodeStatus) -> Self {
        match value {
            NodeStatus::Live => Self::Live,
            NodeStatus::Opened => Self::Opened,
            NodeStatus::Closed => Self::Closed,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TreeRenderRow {
    id: NodeId,
    status: NodeStatus,
    summary: String,
    memory_path: Option<PathBuf>,
    trajs_path: Option<PathBuf>,
    accounting: Option<NodeAccounting>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct NodeAccounting {
    live_context_tokens: Option<i64>,
    live_input_tokens: Option<i64>,
    memory_output_tokens: Option<i64>,
}

fn root_epoch_from_nodes(nodes: &[SpineTreeNode]) -> Option<NodeId> {
    nodes.iter().find_map(root_epoch_from_node)
}

fn root_epoch_from_node(node: &SpineTreeNode) -> Option<NodeId> {
    match node {
        SpineTreeNode::MsgAsLeafNode { .. } => None,
        SpineTreeNode::SpineTree { meta, .. } => meta.id.0.first().copied().map(NodeId::root_epoch),
    }
}

fn project_current_root_epoch_row(cursor: NodeId, rows: &mut Vec<TreeRenderRow>) {
    let Some(root) = cursor.0.first().copied().map(NodeId::root_epoch) else {
        return;
    };
    if rows.iter().any(|row| row.id == root) {
        return;
    }
    rows.push(TreeRenderRow {
        id: root,
        status: NodeStatus::Opened,
        summary: String::new(),
        memory_path: None,
        trajs_path: None,
        accounting: None,
    });
}

fn mark_cursor_statuses(cursor: NodeId, rows: &mut [TreeRenderRow]) {
    for row in rows {
        if row.id == cursor {
            row.status = NodeStatus::Live;
        } else if row.status == NodeStatus::Live || node_is_ancestor_of(&row.id, &cursor) {
            row.status = NodeStatus::Opened;
        }
    }
}

fn node_is_ancestor_of(ancestor: &NodeId, descendant: &NodeId) -> bool {
    ancestor.0.len() < descendant.0.len() && descendant.0.starts_with(ancestor.0.as_slice())
}

impl TreeRenderRow {
    fn snapshot_status(&self) -> SpineTreeNodeStatus {
        if self.status == NodeStatus::Closed
            && self.id.is_root_epoch()
            && self.memory_path.is_some()
            && self.trajs_path.is_none()
        {
            return SpineTreeNodeStatus::Compacted;
        }
        self.status.into()
    }
}

fn collect_tree_render_rows(
    symbols: &[Symbol],
    rows: &mut Vec<TreeRenderRow>,
) -> Result<(), SpineError> {
    for symbol in symbols {
        match symbol {
            Symbol::Control(ControlSymbol::Init(_))
            | Symbol::Control(ControlSymbol::End)
            | Symbol::Control(ControlSymbol::Close(_))
            | Symbol::Control(ControlSymbol::Compact(_, _, _, _)) => {}
            Symbol::Control(ControlSymbol::Open(meta)) => {
                rows.push(TreeRenderRow {
                    id: meta.id.clone(),
                    status: NodeStatus::Live,
                    summary: meta.summary.clone(),
                    memory_path: None,
                    trajs_path: None,
                    accounting: None,
                });
            }
            Symbol::SpineTreeNode(node) => {
                collect_tree_render_node(node, rows)?;
            }
            Symbol::SpineTreeNodes(nodes) => {
                for node in nodes {
                    collect_tree_render_node(node, rows)?;
                }
            }
            Symbol::RootEpoches(root_epochs) => {
                for root_epoch in root_epochs {
                    rows.push(TreeRenderRow {
                        id: root_epoch.memory.node_id.clone(),
                        status: NodeStatus::Closed,
                        summary: String::new(),
                        memory_path: Some(root_epoch.memory.body_path.clone()),
                        trajs_path: None,
                        accounting: memory_accounting(&root_epoch.memory),
                    });
                }
            }
        }
    }

    Ok(())
}

fn collect_tree_render_node(
    node: &SpineTreeNode,
    rows: &mut Vec<TreeRenderRow>,
) -> Result<(), SpineError> {
    match node {
        SpineTreeNode::MsgAsLeafNode { .. } => {}
        SpineTreeNode::SpineTree {
            memory,
            meta,
            children,
            memory_path,
            trajs_path,
            ..
        } => {
            rows.push(TreeRenderRow {
                id: meta.id.clone(),
                status: NodeStatus::Closed,
                summary: meta.summary.clone(),
                memory_path: Some(memory_path.clone()),
                trajs_path: Some(trajs_path.clone()),
                accounting: memory_accounting(memory),
            });
            for child in children {
                collect_tree_render_node(child, rows)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn parse_stack_msg_leaf_count(symbols: &[Symbol]) -> usize {
    symbols
        .iter()
        .map(|symbol| match symbol {
            Symbol::SpineTreeNode(node) => spine_tree_node_msg_leaf_count(node),
            Symbol::SpineTreeNodes(nodes) => nodes.iter().map(spine_tree_node_msg_leaf_count).sum(),
            Symbol::Control(_) | Symbol::RootEpoches(_) => 0,
        })
        .sum()
}

#[cfg(test)]
fn spine_tree_node_msg_leaf_count(node: &SpineTreeNode) -> usize {
    match node {
        SpineTreeNode::MsgAsLeafNode { .. } => 1,
        SpineTreeNode::SpineTree { children, .. } => {
            children.iter().map(spine_tree_node_msg_leaf_count).sum()
        }
    }
}

fn format_tree_rows(rows: Vec<TreeRenderRow>, current_annotation: Option<&str>) -> String {
    let rows = rows
        .into_iter()
        .map(|row| (row.id.clone(), row))
        .collect::<BTreeMap<_, _>>();
    let mut visible = BTreeSet::<NodeId>::new();
    let Some(cursor) = rows
        .values()
        .find(|row| row.status == NodeStatus::Live)
        .map(|row| row.id.clone())
    else {
        return String::new();
    };

    let mut path = Vec::<NodeId>::new();
    let mut current = Some(cursor.clone());
    while let Some(id) = current {
        path.push(id.clone());
        current = id.parent();
    }
    path.reverse();

    for node_id in &path {
        visible.insert(node_id.clone());
        if node_id.is_root_epoch() {
            for sibling in rows.keys() {
                if sibling.is_root_epoch() && sibling < node_id {
                    visible.insert(sibling.clone());
                }
            }
        }
        if let Some(parent) = node_id.parent() {
            for sibling in rows.keys() {
                if sibling.parent() == Some(parent.clone()) && sibling < node_id {
                    visible.insert(sibling.clone());
                }
            }
        }
    }
    for (id, row) in &rows {
        if id.parent().as_ref() == Some(&cursor) && row.status == NodeStatus::Closed {
            visible.insert(id.clone());
        }
    }

    let mut lines = vec![
        format!("Cursor: {cursor}"),
        String::new(),
        "Spine Task Tree:".to_string(),
    ];
    for (id, row) in rows {
        if !visible.contains(&id) {
            continue;
        }
        let marker = match row.status {
            NodeStatus::Live => "Current",
            NodeStatus::Opened => "Open",
            NodeStatus::Closed => "Done",
        };
        let mut detail = String::new();
        if let Some(memory_path) = row.memory_path.as_ref() {
            detail.push_str(&format!(" memory={}", memory_path.display()));
        }
        if let Some(trajs_path) = row.trajs_path.as_ref() {
            detail.push_str(&format!(" trajs={}", trajs_path.display()));
        }
        if let Some(accounting) = row.accounting.as_ref().and_then(format_node_accounting) {
            detail.push_str(&format!(" {accounting}"));
        }
        let summary = visible_summary(&row)
            .map(|summary| format!(" {summary}"))
            .unwrap_or_default();
        let annotation = if row.status == NodeStatus::Live {
            current_annotation
                .map(str::trim)
                .filter(|annotation| !annotation.is_empty())
                .map(|annotation| format!(" {annotation}"))
                .unwrap_or_default()
        } else {
            String::new()
        };
        lines.push(format!(
            "{}- [{}] {}{}{}{}",
            "  ".repeat(id.0.len().saturating_sub(1)),
            id,
            marker,
            summary,
            detail,
            annotation
        ));
    }
    lines.join("\n")
}

fn memory_accounting(memory: &MemoryRef) -> Option<NodeAccounting> {
    Some(NodeAccounting {
        live_context_tokens: memory
            .close_context_tokens
            .zip(memory.open_context_tokens)
            .map(|(close, open)| close.saturating_sub(open))
            .filter(|tokens| *tokens > 0),
        live_input_tokens: memory
            .close_input_tokens
            .zip(memory.open_input_tokens)
            .map(|(close, open)| close.saturating_sub(open))
            .filter(|tokens| *tokens > 0),
        memory_output_tokens: memory.memory_output_tokens.filter(|tokens| *tokens > 0),
    })
    .filter(|accounting| {
        accounting.live_context_tokens.is_some()
            || accounting.live_input_tokens.is_some()
            || accounting.memory_output_tokens.is_some()
    })
}

fn snapshot_accounting(accounting: &NodeAccounting) -> Option<SpineTreeNodeAccountingSnapshot> {
    Some(SpineTreeNodeAccountingSnapshot {
        current_node_context_tokens: None,
        current_node_context_unavailable: None,
        current_node_context_baseline_source: None,
        raw_context_tokens: accounting.live_context_tokens,
        raw_input_tokens: accounting.live_input_tokens,
        memory_output_tokens: accounting.memory_output_tokens,
    })
    .filter(|accounting| {
        accounting.raw_context_tokens.is_some()
            || accounting.raw_input_tokens.is_some()
            || accounting.memory_output_tokens.is_some()
    })
}

fn format_node_accounting(accounting: &NodeAccounting) -> Option<String> {
    let raw_tokens = accounting
        .live_context_tokens
        .or(accounting.live_input_tokens);
    match (raw_tokens, accounting.memory_output_tokens) {
        (Some(raw), Some(memory)) => Some(format!(
            "(~{} raw -> ~{} memory)",
            format_si_suffix(raw),
            format_si_suffix(memory)
        )),
        (Some(raw), None) => Some(format!("(~{} raw)", format_si_suffix(raw))),
        (None, Some(memory)) => Some(format!("(~{} memory)", format_si_suffix(memory))),
        (None, None) => None,
    }
}

fn visible_summary(row: &TreeRenderRow) -> Option<&str> {
    let summary = row.summary.trim();
    if summary.is_empty() || summary == "root" {
        return None;
    }
    Some(summary)
}

pub(super) fn event_to_token(
    event: &LoggedKEvent,
    archive: &SpineArchive,
    mems: &BTreeMap<String, MemRecord>,
    raw_mask: RawMask<'_>,
) -> Result<SpineToken, SpineError> {
    match &event.event {
        KEvent::Init { raw_start } => Ok(SpineToken::Init {
            meta: tree_meta(
                archive,
                NodeId::root_epoch(1),
                *raw_start,
                "root".to_string(),
            )?,
        }),
        KEvent::Msg {
            raw_ordinal,
            context_index,
            from_user,
        } => Ok(SpineToken::Msg {
            seg: SegRef::ResponseItem {
                raw_ordinal: *raw_ordinal,
                context_index: usize::try_from(*context_index)
                    .map_err(|_| SpineError::InvalidEvent("context index overflow".to_string()))?,
            },
            from_user: *from_user,
        }),
        KEvent::Open {
            child,
            index,
            summary,
            open_input_tokens,
            open_context_tokens,
            open_context_source,
            ..
        } => Ok(SpineToken::Open {
            meta: tree_meta_with_token_baselines(
                archive,
                child.clone(),
                *index,
                summary.clone(),
                *open_input_tokens,
                *open_context_tokens,
                *open_context_source,
            )?,
        }),
        KEvent::Close { node, .. } => {
            let mem = mems.values().find(|mem| &mem.node == node).ok_or_else(|| {
                SpineError::InvalidEvent(format!("missing memory for close node {node}"))
            })?;
            if !mem.allowed_by(raw_mask)? {
                return Err(SpineError::InvalidEvent(format!(
                    "memory {} does not cover live raw evidence",
                    mem.compact_id
                )));
            }
            Ok(SpineToken::Close {
                memory: memory_ref(
                    archive,
                    mem.compact_id.clone(),
                    mem.node.clone(),
                    mem.body_hash.clone(),
                    mem.raw_start..mem.raw_end,
                    mem.context_start..mem.context_end,
                    event.seq..event.seq + 1,
                    mem.open_input_tokens,
                    mem.close_input_tokens,
                    mem.open_context_tokens,
                    mem.close_context_tokens,
                    mem.open_context_source,
                    mem.memory_output_tokens,
                ),
            })
        }
        KEvent::RootCompact {
            mem,
            next_open_index,
            next_open_input_tokens,
            next_open_context_tokens,
            ..
        } => {
            let mem = mems.get(mem).ok_or_else(|| {
                SpineError::InvalidEvent("missing memory for root compact".to_string())
            })?;
            if !mem.allowed_by(raw_mask)? {
                return Err(SpineError::InvalidEvent(format!(
                    "memory {} does not cover live raw evidence",
                    mem.compact_id
                )));
            }
            Ok(SpineToken::Compact {
                memory: memory_ref(
                    archive,
                    mem.compact_id.clone(),
                    mem.node.clone(),
                    mem.body_hash.clone(),
                    mem.raw_start..mem.raw_end,
                    mem.context_start..mem.context_end,
                    event.seq..event.seq + 1,
                    mem.open_input_tokens,
                    mem.close_input_tokens,
                    mem.open_context_tokens,
                    mem.close_context_tokens,
                    mem.open_context_source,
                    mem.memory_output_tokens,
                ),
                next_open_index: usize::try_from(*next_open_index).map_err(|_| {
                    SpineError::InvalidEvent("root open index overflow".to_string())
                })?,
                next_open_input_tokens: *next_open_input_tokens,
                next_open_context_tokens: *next_open_context_tokens,
            })
        }
    }
}

pub(super) fn parse_stack_from_events(
    events: &[LoggedKEvent],
    archive: &SpineArchive,
    mems: &[MemRecord],
    raw_mask: RawMask<'_>,
) -> Result<ParseStack, SpineError> {
    let mems = mems
        .iter()
        .cloned()
        .map(|mem| (mem.compact_id.clone(), mem))
        .collect::<BTreeMap<_, _>>();
    let mut ps = ParseStack::new();
    for event in events {
        if !event.allowed_by(raw_mask)? {
            continue;
        }
        ps.shift(event_to_token(event, archive, &mems, raw_mask)?, archive)?;
    }
    Ok(ps)
}
