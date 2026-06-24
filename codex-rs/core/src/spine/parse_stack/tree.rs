use super::ParseStack;
use super::Symbol;
use crate::spine::SpineError;
use crate::spine::model::ControlSymbol;
use crate::spine::model::MemoryRef;
use crate::spine::model::NodeId;
use crate::spine::model::NodeStatus;
use crate::spine::model::SpineTreeNode;
use codex_protocol::num_format::format_si_suffix;
use codex_protocol::spine_tree::SpineTreeNodeAccountingSnapshot;
use codex_protocol::spine_tree::SpineTreeNodeSnapshot;
use codex_protocol::spine_tree::SpineTreeNodeStatus;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::PathBuf;

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
    closed_source_suffix_tokens: Option<i64>,
    closed_memory_context_tokens: Option<i64>,
    memory_output_tokens: Option<i64>,
}

impl NodeAccounting {
    fn is_empty(&self) -> bool {
        self.closed_source_suffix_tokens.is_none()
            && self.closed_memory_context_tokens.is_none()
            && self.memory_output_tokens.is_none()
    }
}

#[cfg(test)]
pub(super) fn render_tree(parse_stack: &ParseStack) -> Result<String, SpineError> {
    let rows = tree_rows(parse_stack)?;
    Ok(format_tree_rows(rows, &BTreeMap::new()))
}

pub(super) fn render_tree_with_context_annotations(
    parse_stack: &ParseStack,
    annotations: &BTreeMap<NodeId, String>,
) -> Result<String, SpineError> {
    let rows = tree_rows(parse_stack)?;
    Ok(format_tree_rows(rows, annotations))
}

pub(super) fn tree_snapshot_nodes(
    parse_stack: &ParseStack,
) -> Result<Vec<SpineTreeNodeSnapshot>, SpineError> {
    let rows = tree_rows(parse_stack)?;
    let rows = tree_rows_by_id(rows);
    let Some((_, projected_ids)) = visible_tree_row_ids(&rows) else {
        return Ok(Vec::new());
    };
    Ok(rows
        .into_iter()
        .filter(|(id, _)| projected_ids.contains(id))
        .map(|(_, row)| {
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
                accounting: row.accounting.as_ref().map(snapshot_accounting),
            }
        })
        .collect())
}

pub(super) fn next_child_id(parse_stack: &ParseStack) -> Result<NodeId, SpineError> {
    let parent = parse_stack.current_cursor_id()?;
    let rows = tree_rows(parse_stack)?;
    let child_count = rows
        .iter()
        .filter(|row| row.id.parent().as_ref() == Some(&parent))
        .count();
    let index = u32::try_from(child_count + 1)
        .map_err(|_| SpineError::InvalidEvent("too many child nodes".to_string()))?;
    Ok(parent.child(index))
}

fn tree_rows(parse_stack: &ParseStack) -> Result<Vec<TreeRenderRow>, SpineError> {
    let mut rows = Vec::<TreeRenderRow>::new();
    collect_tree_render_rows(&parse_stack.symbols, &mut rows);
    let cursor = parse_stack.current_cursor_id()?;
    project_current_root_epoch_row(&cursor, &mut rows);
    mark_cursor_statuses(&cursor, &mut rows);
    Ok(rows)
}

pub(super) fn root_epoch_from_nodes(nodes: &[SpineTreeNode]) -> Option<NodeId> {
    nodes.iter().find_map(root_epoch_from_node)
}

pub(super) fn root_epoch_from_node(node: &SpineTreeNode) -> Option<NodeId> {
    match node {
        SpineTreeNode::MsgAsLeafNode { .. } | SpineTreeNode::ToolCallAsLeafNode { .. } => None,
        SpineTreeNode::SpineTree { meta, .. } => meta.id.0.first().copied().map(NodeId::root_epoch),
    }
}

fn project_current_root_epoch_row(cursor: &NodeId, rows: &mut Vec<TreeRenderRow>) {
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

fn mark_cursor_statuses(cursor: &NodeId, rows: &mut [TreeRenderRow]) {
    for row in rows {
        if &row.id == cursor {
            row.status = NodeStatus::Live;
        } else if row.status == NodeStatus::Live || node_is_ancestor_of(&row.id, cursor) {
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

fn collect_tree_render_rows(symbols: &[Symbol], rows: &mut Vec<TreeRenderRow>) {
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
                collect_tree_render_node(node, rows);
            }
            Symbol::SpineTreeNodes(nodes) => {
                for node in nodes {
                    collect_tree_render_node(node, rows);
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
}

fn collect_tree_render_node(node: &SpineTreeNode, rows: &mut Vec<TreeRenderRow>) {
    match node {
        SpineTreeNode::MsgAsLeafNode { .. } | SpineTreeNode::ToolCallAsLeafNode { .. } => {}
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
                collect_tree_render_node(child, rows);
            }
        }
    }
}

#[cfg(test)]
pub(in crate::spine) fn parse_stack_msg_leaf_count(symbols: &[Symbol]) -> usize {
    parse_stack_leaf_count(symbols, LeafKind::Msg)
}

#[cfg(test)]
pub(in crate::spine) fn parse_stack_toolcall_leaf_count(symbols: &[Symbol]) -> usize {
    parse_stack_leaf_count(symbols, LeafKind::ToolCall)
}

#[cfg(test)]
#[derive(Clone, Copy)]
enum LeafKind {
    Msg,
    ToolCall,
}

#[cfg(test)]
fn parse_stack_leaf_count(symbols: &[Symbol], kind: LeafKind) -> usize {
    symbols
        .iter()
        .map(|symbol| match symbol {
            Symbol::SpineTreeNode(node) => spine_tree_node_leaf_count(node, kind),
            Symbol::SpineTreeNodes(nodes) => nodes
                .iter()
                .map(|node| spine_tree_node_leaf_count(node, kind))
                .sum(),
            Symbol::Control(_) | Symbol::RootEpoches(_) => 0,
        })
        .sum()
}

#[cfg(test)]
fn spine_tree_node_leaf_count(node: &SpineTreeNode, kind: LeafKind) -> usize {
    match node {
        SpineTreeNode::MsgAsLeafNode { .. } => usize::from(matches!(kind, LeafKind::Msg)),
        SpineTreeNode::ToolCallAsLeafNode { .. } => usize::from(matches!(kind, LeafKind::ToolCall)),
        SpineTreeNode::SpineTree { children, .. } => children
            .iter()
            .map(|child| spine_tree_node_leaf_count(child, kind))
            .sum(),
    }
}

fn format_tree_rows(
    rows: Vec<TreeRenderRow>,
    context_annotations: &BTreeMap<NodeId, String>,
) -> String {
    let rows = tree_rows_by_id(rows);
    let Some((cursor, visible)) = visible_tree_row_ids(&rows) else {
        return String::new();
    };

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
        if let Some(accounting) = row.accounting.as_ref().map(format_node_accounting) {
            detail.push_str(&format!(" {accounting}"));
        }
        let summary = visible_summary(&row)
            .map(|summary| format!(" {summary}"))
            .unwrap_or_default();
        let annotation = context_annotations
            .get(&id)
            .map(|annotation| annotation.trim())
            .filter(|annotation| !annotation.is_empty())
            .map(|annotation| format!(" {annotation}"))
            .unwrap_or_default();
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

fn tree_rows_by_id(rows: Vec<TreeRenderRow>) -> BTreeMap<NodeId, TreeRenderRow> {
    rows.into_iter()
        .map(|row| (row.id.clone(), row))
        .collect::<BTreeMap<_, _>>()
}

fn visible_tree_row_ids(
    rows: &BTreeMap<NodeId, TreeRenderRow>,
) -> Option<(NodeId, BTreeSet<NodeId>)> {
    let cursor = rows
        .values()
        .find(|row| row.status == NodeStatus::Live)
        .map(|row| row.id.clone())?;
    let mut visible = BTreeSet::<NodeId>::new();
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
            visible.extend(
                rows.keys()
                    .filter(|sibling| sibling.is_root_epoch() && *sibling < node_id)
                    .cloned(),
            );
        }
        if let Some(parent) = node_id.parent() {
            visible.extend(
                rows.keys()
                    .filter(|sibling| {
                        sibling.parent().as_ref() == Some(&parent) && *sibling < node_id
                    })
                    .cloned(),
            );
        }
    }
    for (id, row) in rows {
        if id.parent().as_ref() == Some(&cursor) && row.status == NodeStatus::Closed {
            visible.insert(id.clone());
        }
    }

    Some((cursor, visible))
}

fn memory_accounting(memory: &MemoryRef) -> Option<NodeAccounting> {
    Some(NodeAccounting {
        closed_source_suffix_tokens: memory.closed_source_suffix_tokens,
        closed_memory_context_tokens: memory.closed_memory_context_tokens,
        memory_output_tokens: memory.memory_output_tokens.filter(|tokens| *tokens > 0),
    })
    .filter(|accounting| !accounting.is_empty())
}

fn snapshot_accounting(accounting: &NodeAccounting) -> SpineTreeNodeAccountingSnapshot {
    SpineTreeNodeAccountingSnapshot {
        current_node_context_tokens: None,
        current_node_context_problem: None,
        current_node_context_baseline_source: None,
        closed_source_suffix_tokens: accounting.closed_source_suffix_tokens,
        closed_memory_context_tokens: accounting.closed_memory_context_tokens,
        memory_output_tokens: accounting.memory_output_tokens,
    }
}

fn format_node_accounting(accounting: &NodeAccounting) -> String {
    match (
        accounting.closed_source_suffix_tokens,
        accounting.closed_memory_context_tokens,
        accounting.memory_output_tokens,
    ) {
        (Some(source), Some(memory), _) => format!(
            "(~{} source -> ~{} memory context)",
            format_si_suffix(source),
            format_si_suffix(memory)
        ),
        (Some(source), None, Some(output)) => format!(
            "(~{} source -> ~{} memory output)",
            format_si_suffix(source),
            format_si_suffix(output)
        ),
        (Some(source), None, None) => format!("(~{} source)", format_si_suffix(source)),
        (None, Some(memory), _) => format!("(~{} memory context)", format_si_suffix(memory)),
        (None, None, Some(output)) => format!("(~{} memory output)", format_si_suffix(output)),
        (None, None, None) => unreachable!("empty NodeAccounting is filtered at construction"),
    }
}

fn visible_summary(row: &TreeRenderRow) -> Option<&str> {
    let summary = row.summary.trim();
    (!summary.is_empty() && summary != "root").then_some(summary)
}
