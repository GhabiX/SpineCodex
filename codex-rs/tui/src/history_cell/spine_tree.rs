use crate::render::line_utils::push_owned_lines;
use crate::wrapping::RtOptions;
use crate::wrapping::adaptive_wrap_line;
use codex_app_server_protocol::SpineTreeNode;
use codex_app_server_protocol::SpineTreeNodeStatus;
use codex_app_server_protocol::SpineTreeUpdatedNotification;
use ratatui::prelude::*;
use ratatui::style::Stylize;

pub(super) fn tree_display_lines(
    snapshot: &SpineTreeUpdatedNotification,
    width: u16,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = vec![vec!["• ".dim(), "Spine Tree".bold()].into()];
    let root_nodes = spine_tree_child_nodes(snapshot, None);
    if root_nodes.is_empty() {
        lines.push(vec!["  └ ".dim(), "(empty)".dim().italic()].into());
        return lines;
    }

    let root_count = root_nodes.len();
    for (index, node) in root_nodes.into_iter().enumerate() {
        render_spine_tree_node(
            snapshot,
            node,
            0,
            index + 1 == root_count,
            &snapshot.active_node_id,
            width,
            &mut lines,
        );
    }
    lines
}

pub(super) fn tree_raw_lines(
    turn_id: &str,
    snapshot: &SpineTreeUpdatedNotification,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(format!(
        "Spine Tree turn={} snapshot_seq={}",
        turn_id, snapshot.snapshot_seq
    ))];
    for node in spine_tree_display_nodes(snapshot) {
        let prefix = "  ".repeat(node.depth);
        let marker = if node.node.node_id == snapshot.active_node_id {
            "* "
        } else {
            ""
        };
        let summary = node
            .node
            .summary
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty());
        let mut line = format!("{prefix}{marker}{}", node.node.node_id);
        if let Some(summary) = summary {
            line.push(' ');
            line.push_str(summary);
        }
        lines.push(Line::from(line));
    }
    lines
}

struct SpineTreeDisplayNode<'a> {
    node: &'a SpineTreeNode,
    depth: usize,
}

fn spine_tree_display_nodes(
    snapshot: &SpineTreeUpdatedNotification,
) -> Vec<SpineTreeDisplayNode<'_>> {
    let mut out = Vec::new();
    append_spine_tree_children(snapshot, None, 0, &mut out);
    out
}

fn append_spine_tree_children<'a>(
    snapshot: &'a SpineTreeUpdatedNotification,
    parent_id: Option<&str>,
    depth: usize,
    out: &mut Vec<SpineTreeDisplayNode<'a>>,
) {
    let children = snapshot
        .nodes
        .iter()
        .filter(|node| node.parent_id.as_deref() == parent_id)
        .collect::<Vec<_>>();
    for node in children {
        out.push(SpineTreeDisplayNode { node, depth });
        append_spine_tree_children(snapshot, Some(node.node_id.as_str()), depth + 1, out);
    }
}

fn spine_tree_child_nodes<'a>(
    snapshot: &'a SpineTreeUpdatedNotification,
    parent_id: Option<&str>,
) -> Vec<&'a SpineTreeNode> {
    snapshot
        .nodes
        .iter()
        .filter(|node| node.parent_id.as_deref() == parent_id)
        .collect()
}

fn render_spine_tree_node(
    snapshot: &SpineTreeUpdatedNotification,
    node: &SpineTreeNode,
    depth: usize,
    is_last: bool,
    active_node_id: &str,
    width: u16,
    out: &mut Vec<Line<'static>>,
) {
    let branch = spine_tree_branch(depth, is_last);
    let prefix = format!("  {}{}", "  ".repeat(depth), branch);
    let active = node.node_id == active_node_id;
    let summary = node
        .summary
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty());
    let mut spans = vec![
        Span::from(prefix.clone()).dim(),
        Span::from(node.node_id.clone()).cyan().bold(),
    ];
    if let Some(summary) = summary {
        spans.push(Span::from(" "));
        spans.push(Span::from(summary.to_string()));
    }
    if active {
        spans.push(Span::from(" current").cyan().bold());
    } else {
        spans.push(Span::from(" "));
        spans.push(Span::from(spine_tree_node_status_label(node.status)).dim());
    }
    let line = Line::from(spans);
    let wrapped = adaptive_wrap_line(
        &line,
        RtOptions::new(width.saturating_sub(2).max(1) as usize)
            .subsequent_indent(format!("{}  ", "  ".repeat(depth + 1)).into()),
    );
    push_owned_lines(&wrapped, out);

    let real_children = spine_tree_child_nodes(snapshot, Some(node.node_id.as_str()));
    let real_child_count = real_children.len();
    for (index, child) in real_children.into_iter().enumerate() {
        render_spine_tree_node(
            snapshot,
            child,
            depth + 1,
            index + 1 == real_child_count,
            active_node_id,
            width,
            out,
        );
    }
}

fn spine_tree_branch(_depth: usize, is_last: bool) -> &'static str {
    if is_last { "└ " } else { "├ " }
}

fn spine_tree_node_status_label(status: SpineTreeNodeStatus) -> &'static str {
    match status {
        SpineTreeNodeStatus::Live => "live",
        SpineTreeNodeStatus::Suspended => "suspended",
        SpineTreeNodeStatus::Closed => "closed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_lines(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    fn snapshot(active_node_id: &str, nodes: Vec<SpineTreeNode>) -> SpineTreeUpdatedNotification {
        SpineTreeUpdatedNotification {
            thread_id: "thread-spine-tree".to_string(),
            turn_id: "turn-spine-tree".to_string(),
            snapshot_seq: 1,
            active_node_id: active_node_id.to_string(),
            nodes,
        }
    }

    fn node(
        node_id: &str,
        parent_id: Option<&str>,
        summary: Option<&str>,
        status: SpineTreeNodeStatus,
    ) -> SpineTreeNode {
        SpineTreeNode {
            node_id: node_id.to_string(),
            parent_id: parent_id.map(str::to_string),
            summary: summary.map(str::to_string),
            status,
        }
    }

    #[test]
    fn tree_display_wraps_narrow_width_without_losing_labels() {
        let snapshot = snapshot(
            "1.1",
            vec![
                node(
                    "1",
                    None,
                    Some("root scope"),
                    SpineTreeNodeStatus::Suspended,
                ),
                node(
                    "1.1",
                    Some("1"),
                    Some("focused scope with narrow wrapping visible token"),
                    SpineTreeNodeStatus::Live,
                ),
            ],
        );

        let rendered = render_lines(&tree_display_lines(&snapshot, /*width*/ 22));
        let joined = rendered.join("\n");

        assert!(
            rendered.len() > 4,
            "expected narrow rendering to wrap, got: {rendered:?}"
        );
        assert!(joined.contains("Spine Tree"));
        assert!(joined.contains("1.1"));
        assert!(joined.contains("narrow"));
    }

    #[test]
    fn tree_display_handles_deep_tree_and_missing_summary() {
        let snapshot = snapshot(
            "1.1.1.1",
            vec![
                node("1", None, None, SpineTreeNodeStatus::Suspended),
                node("1.1", Some("1"), Some(" "), SpineTreeNodeStatus::Suspended),
                node(
                    "1.1.1",
                    Some("1.1"),
                    Some("nested scope"),
                    SpineTreeNodeStatus::Closed,
                ),
                node("1.1.1.1", Some("1.1.1"), None, SpineTreeNodeStatus::Live),
            ],
        );

        let rendered = render_lines(&tree_display_lines(&snapshot, /*width*/ 80)).join("\n");

        assert!(rendered.contains("1 suspended"));
        assert!(rendered.contains("1.1 suspended"));
        assert!(rendered.contains("1.1.1 nested scope closed"));
        assert!(rendered.contains("1.1.1.1 current"));
    }

    #[test]
    fn tree_display_shows_node_status_and_root_branches() {
        let snapshot = snapshot(
            "2.1",
            vec![
                node(
                    "1",
                    None,
                    Some("archived epoch"),
                    SpineTreeNodeStatus::Closed,
                ),
                node(
                    "2",
                    None,
                    Some("current epoch"),
                    SpineTreeNodeStatus::Suspended,
                ),
                node(
                    "2.1",
                    Some("2"),
                    Some("focused leaf"),
                    SpineTreeNodeStatus::Live,
                ),
            ],
        );

        let rendered = render_lines(&tree_display_lines(&snapshot, /*width*/ 80));

        assert!(
            rendered[1].starts_with("  ├ 1 archived epoch closed"),
            "expected first root epoch to use a non-last branch and show status, got: {rendered:?}"
        );
        assert!(
            rendered[2].starts_with("  └ 2 current epoch suspended"),
            "expected second root epoch to use a last branch and show status, got: {rendered:?}"
        );
        assert!(rendered[3].contains("2.1 focused leaf current"));
    }
}
