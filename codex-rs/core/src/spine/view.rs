use super::ids::NodeId;
use super::runtime::SpineRuntimeHint;
use super::state::NodeStatus;
use super::state::SpineState;
use super::store::SpineOperation;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;

pub(crate) struct SpineContextBudgetHint {
    pub(crate) used_tokens: u64,
    pub(crate) limit_tokens: u64,
}

pub(crate) fn render_tool_output(
    _op: SpineOperation,
    state: &SpineState,
    cursor: &NodeId,
) -> String {
    render_spine_tree_view(state, cursor)
}

pub(crate) fn render_tool_output_with_base(
    _op: SpineOperation,
    state: &SpineState,
    cursor: &NodeId,
    base: &Path,
) -> String {
    render_spine_tree_view_with_base(state, cursor, Some(base))
}

pub(crate) fn render_tree_tool_output(state: &SpineState, cursor: &NodeId) -> String {
    render_spine_tree_view(state, cursor)
}

pub(crate) fn render_tree_tool_output_with_base(
    state: &SpineState,
    cursor: &NodeId,
    base: &Path,
) -> String {
    render_spine_tree_view_with_base(state, cursor, Some(base))
}

fn render_spine_tree_view(state: &SpineState, cursor: &NodeId) -> String {
    render_spine_tree_view_with_base(state, cursor, None)
}

fn render_spine_tree_view_with_base(
    state: &SpineState,
    cursor: &NodeId,
    base: Option<&Path>,
) -> String {
    let base_line = base
        .map(|base| format!("\nBase: {}", base.display()))
        .unwrap_or_default();
    format!(
        "Current:  {}{}\n\n{}",
        display_node_id(cursor),
        base_line,
        render_tree(state, cursor),
    )
}

pub(crate) fn render_size_hint(
    hint: &SpineRuntimeHint,
    budget: Option<&SpineContextBudgetHint>,
) -> String {
    if let Some(budget) = budget {
        let remaining_tokens = budget.limit_tokens.saturating_sub(budget.used_tokens);
        return format!(
            "\n\nSpine hint: context is about {}k/{}k tokens ({}k left); current live node is about {}k. At a natural boundary, use spine.next/close to move finished work into a worklog before Codex auto-compacts the root epoch.",
            rounded_k_tokens(budget.used_tokens),
            rounded_k_tokens(budget.limit_tokens),
            rounded_k_tokens(remaining_tokens),
            rounded_k_tokens(hint.estimated_tokens)
        );
    }
    format!(
        "\n\nSpine hint: current live node is about {}k tokens and is carried into every request. At a natural boundary, use spine.next/close to move finished work into a worklog.",
        rounded_k_tokens(hint.estimated_tokens)
    )
}

fn rounded_k_tokens(tokens: u64) -> u64 {
    tokens.saturating_add(500) / 1_000
}

pub(crate) fn render_tree(state: &SpineState, cursor: &NodeId) -> String {
    let visible = state.visible_spine().into_iter().collect::<HashSet<_>>();
    let active_epoch = root_epoch_for(cursor);
    let previous_epoch = active_epoch.as_ref().and_then(previous_root_epoch);
    let rows = state
        .nodes()
        .iter()
        .filter(|(_, node)| node.parent_id.is_none())
        .map(|(node_id, _)| {
            format_subtree(
                state,
                node_id,
                cursor,
                &visible,
                previous_epoch.as_ref(),
                0,
            )
        })
        .collect::<Vec<_>>();
    if rows.is_empty() {
        "(empty)".to_string()
    } else {
        rows.join("\n")
    }
}

fn format_subtree(
    state: &SpineState,
    node_id: &NodeId,
    cursor: &NodeId,
    visible: &HashSet<NodeId>,
    previous_epoch: Option<&NodeId>,
    depth: usize,
) -> String {
    let node = state
        .node(node_id)
        .expect("formatting an existing spine node");
    let prefix = "    ".repeat(depth);
    let mut line = format!("{}{}:", prefix, display_node_id(node_id));
    if node_id == cursor {
        line.push_str(" Current");
    } else {
        line.push(' ');
        let undone_as_compact = is_unfinished_under_closed_ancestor(state, node_id);
        line.push_str(format_status(&node.status, undone_as_compact));
        if let Some(summary) = node
            .summary
            .as_deref()
            .filter(|summary| !summary.is_empty())
        {
            line.push(' ');
            line.push_str(summary);
        }
        if should_show_worklog_ref(&node.status) {
            line.push(' ');
            if visible.contains(node_id) || Some(node_id) == previous_epoch {
                line.push_str("[worklog already in context]");
            } else {
                line.push_str(&relative_worklog_path(node_id).display().to_string());
            }
        }
    }

    let child_depth = depth + 1;
    let children = state
        .nodes()
        .iter()
        .filter(|(_, child)| child.parent_id.as_ref() == Some(node_id))
        .map(|(child_id, _)| {
            format_subtree(
                state,
                child_id,
                cursor,
                visible,
                previous_epoch,
                child_depth,
            )
        })
        .collect::<Vec<_>>();
    if children.is_empty() {
        line
    } else {
        format!("{line}\n{}", children.join("\n"))
    }
}

fn root_epoch_for(node_id: &NodeId) -> Option<NodeId> {
    let first = *node_id.segments().first()?;
    Some(NodeId::from_segments(vec![first]))
}

fn previous_root_epoch(node_id: &NodeId) -> Option<NodeId> {
    let segments = node_id.segments();
    if segments.len() != 1 || segments[0] <= 1 {
        return None;
    }
    Some(NodeId::from_segments(vec![segments[0] - 1]))
}

fn format_status(status: &NodeStatus, undone_as_compact: bool) -> &'static str {
    if undone_as_compact {
        return "[undone as compact]";
    }
    match status {
        NodeStatus::Live => "live",
        NodeStatus::Opened => "live",
        NodeStatus::Finished => "finished",
        NodeStatus::Closed => "closed",
    }
}

fn is_unfinished_under_closed_ancestor(state: &SpineState, node_id: &NodeId) -> bool {
    let Some(node) = state.node(node_id) else {
        return false;
    };
    if !matches!(node.status, NodeStatus::Live | NodeStatus::Opened) {
        return false;
    }

    let mut parent_id = node.parent_id.as_ref();
    while let Some(parent) = parent_id {
        let Some(parent_node) = state.node(parent) else {
            return false;
        };
        if matches!(parent_node.status, NodeStatus::Closed) {
            return true;
        }
        parent_id = parent_node.parent_id.as_ref();
    }
    false
}

fn should_show_worklog_ref(status: &NodeStatus) -> bool {
    matches!(status, NodeStatus::Finished | NodeStatus::Closed)
}

pub(crate) fn display_node_id(node_id: &NodeId) -> String {
    node_id.to_string()
}

pub(crate) fn op_label(op: SpineOperation) -> &'static str {
    match op {
        SpineOperation::Open => "open",
        SpineOperation::Next => "next",
        SpineOperation::Close => "close",
        SpineOperation::Archive => "archive",
    }
}

pub(crate) fn relative_worklog_path(node_id: &NodeId) -> PathBuf {
    let mut path = PathBuf::from("nodes");
    for segment in node_id.segments() {
        path.push(segment.to_string());
    }
    path.push("worklog.md");
    path
}

pub(crate) fn relative_node_trajs_path(node_id: &NodeId) -> PathBuf {
    let mut path = PathBuf::from("nodes");
    for segment in node_id.segments() {
        path.push(segment.to_string());
    }
    path.push("trajs.jsonl");
    path
}

#[cfg(test)]
#[path = "view_tests.rs"]
mod tests;
