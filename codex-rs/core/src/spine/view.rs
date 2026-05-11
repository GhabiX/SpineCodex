use super::ids::NodeId;
use super::state::NodeStatus;
use super::state::SpineState;
use super::store::SpineOperation;
use std::path::PathBuf;

pub(crate) fn render_tool_output(
    op: SpineOperation,
    state: &SpineState,
    cursor: &NodeId,
) -> String {
    let cursor_status = state
        .node(cursor)
        .map(|node| format_status(&node.status))
        .unwrap_or("unknown");
    format!(
        "Spine updated: {}\n\ncurrent: {} {}\n\n{}",
        op_label(op),
        cursor.bracketed(),
        cursor_status,
        render_tree(state, cursor)
    )
}

pub(crate) fn render_tree(state: &SpineState, cursor: &NodeId) -> String {
    state
        .nodes()
        .iter()
        .filter(|(_, node)| node.parent_id.is_none())
        .map(|(node_id, _)| format_subtree(state, node_id, cursor, 0, true))
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_subtree(
    state: &SpineState,
    node_id: &NodeId,
    cursor: &NodeId,
    depth: usize,
    is_root: bool,
) -> String {
    let node = state
        .node(node_id)
        .expect("formatting an existing spine node");
    let prefix = if is_root {
        String::new()
    } else {
        format!("{}|-- ", "    ".repeat(depth.saturating_sub(1)))
    };
    let summary = node
        .summary
        .as_deref()
        .or_else(|| (node_id == cursor).then_some("current"))
        .unwrap_or("");
    let mut line = format!(
        "{}{} {}",
        prefix,
        node_id.bracketed(),
        format_status(&node.status)
    );
    if !summary.is_empty() {
        line.push(' ');
        line.push_str(summary);
    }
    if node_id == cursor && summary != "current" {
        line.push_str(" current");
    }
    line.push_str(&format!(" ({})", relative_worklog_path(node_id).display()));

    let child_depth = depth + 1;
    let children = state
        .nodes()
        .iter()
        .filter(|(_, child)| child.parent_id.as_ref() == Some(node_id))
        .map(|(child_id, _)| format_subtree(state, child_id, cursor, child_depth, false))
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
        NodeStatus::Opened => "opened",
        NodeStatus::Finished => "finished",
        NodeStatus::Closed => "closed",
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
