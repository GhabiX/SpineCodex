//! Host-only Spine tree projection history cell.

use super::*;
use codex_app_server_protocol::SpineTreeNode;
use codex_app_server_protocol::SpineTreeNodeStatus;
use codex_app_server_protocol::SpineTreeUpdatedNotification;

pub(crate) fn new_spine_tree_update(
    turn_id: String,
    snapshot: SpineTreeUpdatedNotification,
) -> SpineTreeUpdateCell {
    SpineTreeUpdateCell {
        turn_id,
        snapshot,
        source: SpineTreeUpdateSource::Live,
    }
}

pub(crate) fn new_manual_spine_tree_snapshot(
    snapshot: SpineTreeUpdatedNotification,
) -> SpineTreeUpdateCell {
    SpineTreeUpdateCell {
        turn_id: snapshot.turn_id.clone(),
        snapshot,
        source: SpineTreeUpdateSource::Manual,
    }
}

#[derive(Debug)]
pub(crate) struct SpineTreeUpdateCell {
    turn_id: String,
    snapshot: SpineTreeUpdatedNotification,
    source: SpineTreeUpdateSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpineTreeUpdateSource {
    Live,
    Manual,
}

impl SpineTreeUpdateCell {
    pub(crate) fn turn_id(&self) -> &str {
        &self.turn_id
    }

    pub(crate) fn snapshot_seq(&self) -> u64 {
        self.snapshot.snapshot_seq
    }

    pub(crate) fn is_live_update(&self) -> bool {
        self.source == SpineTreeUpdateSource::Live
    }
}

impl HistoryCell for SpineTreeUpdateCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines = vec![vec!["• ".dim(), "Spine Tree".bold()].into()];
        let root_nodes = child_nodes(&self.snapshot, None);
        if root_nodes.is_empty() {
            lines.push(vec!["  └ ".dim(), "(empty)".dim().italic()].into());
            return lines;
        }

        let root_count = root_nodes.len();
        for (index, node) in root_nodes.into_iter().enumerate() {
            render_node(
                &self.snapshot,
                node,
                0,
                index + 1 == root_count,
                width,
                &mut lines,
            );
        }
        lines
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        let mut lines = vec![Line::from("Spine Tree")];
        append_raw_children(&self.snapshot, None, 0, &mut lines);
        lines
    }
}

fn render_node(
    snapshot: &SpineTreeUpdatedNotification,
    node: &SpineTreeNode,
    depth: usize,
    is_last: bool,
    width: u16,
    out: &mut Vec<Line<'static>>,
) {
    let active = node.node_id == snapshot.active_node_id;
    let status = if active {
        "current"
    } else {
        status_label(node.status)
    };
    let mut spans = vec![
        Span::from(format!("  {}{}", "  ".repeat(depth), branch(is_last))).dim(),
        Span::from(node.node_id.clone()).cyan().bold(),
    ];
    if let Some(summary) = node
        .summary
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        spans.push(Span::from(" "));
        spans.push(Span::from(summary.to_string()));
    }
    spans.push(Span::from(" "));
    let status_span = if active {
        Span::from(status.to_string()).cyan().bold()
    } else if node.status == SpineTreeNodeStatus::Compacted {
        Span::from(status.to_string()).yellow().bold()
    } else {
        Span::from(status.to_string()).dim()
    };
    spans.push(status_span);

    let line = Line::from(spans);
    let wrapped = adaptive_wrap_line(
        &line,
        RtOptions::new(width.saturating_sub(2).max(1) as usize)
            .subsequent_indent(format!("{}  ", "  ".repeat(depth + 1)).into()),
    );
    push_owned_lines(&wrapped, out);

    let children = child_nodes(snapshot, Some(node.node_id.as_str()));
    let child_count = children.len();
    for (index, child) in children.into_iter().enumerate() {
        render_node(
            snapshot,
            child,
            depth + 1,
            index + 1 == child_count,
            width,
            out,
        );
    }
}

fn append_raw_children(
    snapshot: &SpineTreeUpdatedNotification,
    parent_id: Option<&str>,
    depth: usize,
    out: &mut Vec<Line<'static>>,
) {
    for node in child_nodes(snapshot, parent_id) {
        let marker = if node.node_id == snapshot.active_node_id {
            "* "
        } else {
            ""
        };
        let summary = node
            .summary
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(|summary| format!(" {summary}"))
            .unwrap_or_default();
        out.push(Line::from(format!(
            "{}{}{}{} {}",
            "  ".repeat(depth),
            marker,
            node.node_id,
            summary,
            if node.node_id == snapshot.active_node_id {
                "current"
            } else {
                status_label(node.status)
            }
        )));
        append_raw_children(snapshot, Some(node.node_id.as_str()), depth + 1, out);
    }
}

fn child_nodes<'a>(
    snapshot: &'a SpineTreeUpdatedNotification,
    parent_id: Option<&str>,
) -> Vec<&'a SpineTreeNode> {
    snapshot
        .nodes
        .iter()
        .filter(|node| node.parent_id.as_deref() == parent_id)
        .collect()
}

fn branch(is_last: bool) -> &'static str {
    if is_last { "└ " } else { "├ " }
}

fn status_label(status: SpineTreeNodeStatus) -> &'static str {
    match status {
        SpineTreeNodeStatus::Live => "current",
        SpineTreeNodeStatus::Opened => "open",
        SpineTreeNodeStatus::Closed => "done",
        SpineTreeNodeStatus::Compacted => "compacted",
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

    fn snapshot(nodes: Vec<SpineTreeNode>) -> SpineTreeUpdatedNotification {
        snapshot_with_active("2.1", nodes)
    }

    fn snapshot_with_active(
        active_node_id: &str,
        nodes: Vec<SpineTreeNode>,
    ) -> SpineTreeUpdatedNotification {
        SpineTreeUpdatedNotification {
            thread_id: "thread".to_string(),
            turn_id: "turn".to_string(),
            snapshot_seq: 7,
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
    fn renders_visible_tree_without_internal_terms() {
        let cell = new_spine_tree_update(
            "turn".to_string(),
            snapshot(vec![
                node("1", None, Some("earlier work"), SpineTreeNodeStatus::Closed),
                node("2", None, Some("current root"), SpineTreeNodeStatus::Opened),
                node(
                    "2.1",
                    Some("2"),
                    Some("focused task"),
                    SpineTreeNodeStatus::Live,
                ),
            ]),
        );

        let rendered = render_lines(&cell.display_lines(80)).join("\n");
        assert!(rendered.contains("Spine Tree"));
        assert!(rendered.contains("1 earlier work done"));
        assert!(rendered.contains("2 current root open"));
        assert!(rendered.contains("2.1 focused task current"));
        assert!(!rendered.contains("LR"));
        assert!(!rendered.contains("ParseStack"));
        assert!(!rendered.contains("raw"));
        assert!(!rendered.contains("memory="));
        assert!(!rendered.contains("trajs="));
        assert!(!rendered.contains("PlanTree"));
    }

    #[test]
    fn renders_root_compact_status_distinctly() {
        let cell = new_spine_tree_update(
            "turn".to_string(),
            snapshot(vec![
                node("1", None, Some("old root"), SpineTreeNodeStatus::Compacted),
                node("2", None, Some("new root"), SpineTreeNodeStatus::Opened),
                node("2.1", Some("2"), Some("active"), SpineTreeNodeStatus::Live),
            ]),
        );

        let rendered = render_lines(&cell.display_lines(80)).join("\n");
        assert!(rendered.contains("1 old root compacted"));
        assert!(!rendered.contains("1 old root done"));
    }

    #[test]
    fn renders_promoted_snapshot_root_without_empty_placeholder() {
        let cell = new_spine_tree_update(
            "turn".to_string(),
            snapshot_with_active(
                "1.1.1",
                vec![
                    node("1.1", None, Some("root"), SpineTreeNodeStatus::Opened),
                    node(
                        "1.1.1",
                        Some("1.1"),
                        Some("focused task"),
                        SpineTreeNodeStatus::Live,
                    ),
                ],
            ),
        );

        let rendered = render_lines(&cell.display_lines(80)).join("\n");
        assert!(!rendered.contains("(empty)"));
        assert!(rendered.contains("1.1 root open"));
        assert!(rendered.contains("1.1.1 focused task current"));
    }

    #[test]
    fn renders_active_root_cursor_with_closed_child() {
        let cell = new_spine_tree_update(
            "turn".to_string(),
            snapshot_with_active(
                "1",
                vec![
                    node("1", None, Some("root"), SpineTreeNodeStatus::Live),
                    node("1.1", Some("1"), Some("root"), SpineTreeNodeStatus::Closed),
                ],
            ),
        );

        let rendered = render_lines(&cell.display_lines(80)).join("\n");
        assert!(!rendered.contains("(empty)"));
        assert!(rendered.contains("1 root current"));
        assert!(rendered.contains("1.1 root done"));
    }
}
