use super::ids::NodeId;
use super::state::NodeStatus;
use super::state::SpineState;
use super::store::SpineOperation;
use std::collections::HashSet;
use std::path::PathBuf;

pub(crate) fn render_tool_output(
    _op: SpineOperation,
    state: &SpineState,
    cursor: &NodeId,
) -> String {
    render_spine_tree_view(state, cursor)
}

pub(crate) fn render_tree_tool_output(state: &SpineState, cursor: &NodeId) -> String {
    render_spine_tree_view(state, cursor)
}

fn render_spine_tree_view(state: &SpineState, cursor: &NodeId) -> String {
    format!(
        "Current:  {}\n\n{}",
        display_node_id(cursor),
        render_tree(state, cursor)
    )
}

pub(crate) fn render_tree(state: &SpineState, cursor: &NodeId) -> String {
    let visible = state.visible_spine().into_iter().collect::<HashSet<_>>();
    let rows = state
        .nodes()
        .iter()
        .filter(|(node_id, node)| {
            *node_id != &NodeId::root()
                && (node.parent_id.is_none() || node.parent_id.as_ref() == Some(&NodeId::root()))
        })
        .map(|(node_id, _)| format_subtree(state, node_id, cursor, &visible, 0))
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
        line.push_str(format_status(&node.status));
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
            if visible.contains(node_id) {
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
        .map(|(child_id, _)| format_subtree(state, child_id, cursor, visible, child_depth))
        .collect::<Vec<_>>();
    if children.is_empty() {
        line
    } else {
        format!("{line}\n{}", children.join("\n"))
    }
}

fn format_status(status: &NodeStatus) -> &'static str {
    match status {
        NodeStatus::Live => "live",
        NodeStatus::Opened => "live",
        NodeStatus::Finished => "finished",
        NodeStatus::Closed => "closed",
    }
}

fn should_show_worklog_ref(status: &NodeStatus) -> bool {
    matches!(status, NodeStatus::Finished | NodeStatus::Closed)
}

pub(crate) fn display_node_id(node_id: &NodeId) -> String {
    let segments = node_id.segments();
    let display_segments = if segments == [1] {
        return "root".to_string();
    } else if segments.len() > 1 && segments.first() == Some(&1) {
        &segments[1..]
    } else {
        segments
    };
    if display_segments.is_empty() {
        "root".to_string()
    } else {
        display_segments
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(".")
    }
}

pub(crate) fn op_label(op: SpineOperation) -> &'static str {
    match op {
        SpineOperation::Open => "open",
        SpineOperation::Next => "next",
        SpineOperation::Close => "close",
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
