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
        live: true,
    }
}

pub(crate) fn new_spine_tree_snapshot(
    snapshot: SpineTreeUpdatedNotification,
) -> SpineTreeUpdateCell {
    SpineTreeUpdateCell {
        turn_id: snapshot.turn_id.clone(),
        snapshot,
        live: false,
    }
}

#[derive(Debug)]
pub(crate) struct SpineTreeUpdateCell {
    turn_id: String,
    snapshot: SpineTreeUpdatedNotification,
    live: bool,
}

impl SpineTreeUpdateCell {
    pub(crate) fn turn_id(&self) -> &str {
        &self.turn_id
    }

    pub(crate) fn snapshot_seq(&self) -> u64 {
        self.snapshot.snapshot_seq
    }

    pub(crate) fn is_live_update(&self) -> bool {
        self.live
    }
}

impl HistoryCell for SpineTreeUpdateCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        snapshot_lines(&self.snapshot)
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        snapshot_lines(&self.snapshot)
    }
}

fn snapshot_lines(snapshot: &SpineTreeUpdatedNotification) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        "Spine Tree".bold(),
        "  ".into(),
        format!("current {}", snapshot.active_node_id).cyan().into(),
    ])];
    for node in &snapshot.nodes {
        let depth = node_depth(&snapshot.nodes, node);
        let marker = match node.status {
            SpineTreeNodeStatus::Live => "*",
            SpineTreeNodeStatus::Opened => ">",
            SpineTreeNodeStatus::Closed => "ok",
            SpineTreeNodeStatus::Compacted => "~",
        };
        let active = node.node_id == snapshot.active_node_id;
        let mut label = format!("{}{} {}", "  ".repeat(depth + 1), marker, node.node_id);
        if let Some(summary) = node
            .summary
            .as_deref()
            .filter(|summary| !summary.trim().is_empty())
        {
            label.push(' ');
            label.push_str(summary.trim());
        }
        if active {
            lines.push(Line::from(label.cyan().bold()));
        } else {
            lines.push(Line::from(label));
        }
    }
    lines
}

fn node_depth(nodes: &[SpineTreeNode], node: &SpineTreeNode) -> usize {
    let mut depth = 0;
    let mut parent = node.parent_id.as_deref();
    while let Some(parent_id) = parent {
        depth += 1;
        parent = nodes
            .iter()
            .find(|candidate| candidate.node_id == parent_id)
            .and_then(|candidate| candidate.parent_id.as_deref());
        if depth > nodes.len() {
            break;
        }
    }
    depth
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_app_server_protocol::SpineTreeNodeKind;
    use pretty_assertions::assert_eq;

    #[test]
    fn spine_tree_snapshot_renders_hierarchy_status_and_active_cursor() {
        let cell = new_spine_tree_snapshot(SpineTreeUpdatedNotification {
            thread_id: "thread".to_string(),
            turn_id: "turn".to_string(),
            snapshot_seq: 14,
            active_node_id: "2.2".to_string(),
            nodes: vec![
                node(
                    "1",
                    None,
                    SpineTreeNodeKind::RootEpoch,
                    SpineTreeNodeStatus::Compacted,
                    None,
                ),
                node(
                    "2",
                    None,
                    SpineTreeNodeKind::RootEpoch,
                    SpineTreeNodeStatus::Opened,
                    None,
                ),
                node(
                    "2.1",
                    Some("2"),
                    SpineTreeNodeKind::Task,
                    SpineTreeNodeStatus::Closed,
                    Some("finished"),
                ),
                node(
                    "2.2",
                    Some("2"),
                    SpineTreeNodeKind::Task,
                    SpineTreeNodeStatus::Live,
                    Some("current work"),
                ),
            ],
        });

        let lines = cell
            .display_lines(/*width*/ 80)
            .into_iter()
            .map(|line| {
                line.spans
                    .into_iter()
                    .map(|span| span.content.into_owned())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            lines,
            vec![
                "Spine Tree  current 2.2",
                "  ~ 1",
                "  > 2",
                "    ok 2.1 finished",
                "    * 2.2 current work",
            ]
        );
    }

    fn node(
        node_id: &str,
        parent_id: Option<&str>,
        kind: SpineTreeNodeKind,
        status: SpineTreeNodeStatus,
        summary: Option<&str>,
    ) -> SpineTreeNode {
        SpineTreeNode {
            node_id: node_id.to_string(),
            parent_id: parent_id.map(str::to_string),
            kind,
            status,
            summary: summary.map(str::to_string),
            memory_summary: None,
            start: 0,
            end: None,
        }
    }
}
