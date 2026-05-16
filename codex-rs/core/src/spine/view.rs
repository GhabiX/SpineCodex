use super::ids::NodeId;
use super::ids::NodeIdParseError;
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

pub(crate) fn context_budget_is_under_pressure(budget: &SpineContextBudgetHint) -> bool {
    let remaining_tokens = budget.limit_tokens.saturating_sub(budget.used_tokens);
    u128::from(remaining_tokens) * 4 < u128::from(budget.limit_tokens)
}

pub(crate) fn render_tool_output_with_base(
    _op: SpineOperation,
    state: &SpineState,
    cursor: &NodeId,
    base: &Path,
) -> String {
    render_spine_tree_view_with_base(state, cursor, Some(base))
}

#[cfg(test)]
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

#[cfg(test)]
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
            "\n\nSpine warning: context pressure is high at {}k/{}k tokens ({}k left); current live node is about {}k. At the next natural boundary, use spine.next/close to move finished work into a worklog before Codex auto-compacts the root epoch.",
            rounded_k_tokens(budget.used_tokens),
            rounded_k_tokens(budget.limit_tokens),
            rounded_k_tokens(remaining_tokens),
            rounded_k_tokens(hint.estimated_tokens)
        );
    }
    format!(
        "\n\nSpine warning: current live node is about {}k tokens and is carried into every request. At a natural boundary, use spine.next/close to move finished work into a worklog.",
        rounded_k_tokens(hint.estimated_tokens)
    )
}

fn rounded_k_tokens(tokens: u64) -> u64 {
    tokens.saturating_add(500) / 1_000
}

pub(crate) fn render_tree(state: &SpineState, cursor: &NodeId) -> String {
    let visible = state.visible_spine().into_iter().collect::<HashSet<_>>();
    let current_root_epoch = state.current_root_epoch().ok();
    let previous_root_epoch = current_root_epoch.as_ref().and_then(previous_root_epoch_id);
    let top_level_nodes = state
        .nodes()
        .iter()
        .filter(|(_, node)| node.parent_id.is_none())
        .map(|(node_id, _)| node_id.clone())
        .collect::<Vec<_>>();
    let rows = top_level_nodes
        .iter()
        .map(|node_id| {
            format_subtree(
                state,
                node_id,
                cursor,
                &visible,
                current_root_epoch.as_ref(),
                previous_root_epoch.as_ref(),
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
    current_root_epoch: Option<&NodeId>,
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
        if should_show_worklog_ref(state, node_id, &node.status, current_root_epoch) {
            line.push(' ');
            if worklog_is_already_in_context(
                state,
                node_id,
                visible,
                current_root_epoch,
                previous_epoch,
            ) {
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
                current_root_epoch,
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

fn worklog_is_already_in_context(
    state: &SpineState,
    node_id: &NodeId,
    visible: &HashSet<NodeId>,
    current_root_epoch: Option<&NodeId>,
    previous_root_epoch: Option<&NodeId>,
) -> bool {
    if is_root_epoch(state, node_id) {
        return previous_root_epoch == Some(node_id);
    }

    let Some(root_epoch) = root_epoch_for(node_id) else {
        return false;
    };
    if Some(&root_epoch) != current_root_epoch {
        return false;
    }
    visible.contains(node_id)
}

fn root_epoch_for(node_id: &NodeId) -> Option<NodeId> {
    let first = *node_id.segments().first()?;
    Some(NodeId::from_segments(vec![first]))
}

fn is_root_epoch(state: &SpineState, node_id: &NodeId) -> bool {
    state
        .node(node_id)
        .is_some_and(|node| node.parent_id.is_none())
}

fn previous_root_epoch_id(node_id: &NodeId) -> Option<NodeId> {
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

fn should_show_worklog_ref(
    state: &SpineState,
    node_id: &NodeId,
    status: &NodeStatus,
    current_root_epoch: Option<&NodeId>,
) -> bool {
    if !matches!(status, NodeStatus::Finished | NodeStatus::Closed) {
        return false;
    }
    if is_root_epoch(state, node_id) {
        return true;
    }
    let Some(root_epoch) = root_epoch_for(node_id) else {
        return false;
    };
    Some(&root_epoch) == current_root_epoch
}

pub(crate) fn display_node_id(node_id: &NodeId) -> String {
    if node_id.segments().is_empty() {
        return "root".to_string();
    }
    node_id.to_string()
}

pub(crate) fn parse_display_node_id(value: &str) -> Result<NodeId, NodeIdParseError> {
    NodeId::parse(value)
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
