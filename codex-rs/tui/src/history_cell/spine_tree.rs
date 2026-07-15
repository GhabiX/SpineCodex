use super::*;
use codex_app_server_protocol::SpineTreeNode;
use codex_app_server_protocol::SpineTreeNodeStatus;
use codex_app_server_protocol::SpineTreeUpdatedNotification;
use std::collections::HashSet;

#[path = "spine_tree_debug.rs"]
mod debug;

const PRETTY_MAX_VISIBLE_SIBLINGS: usize = 3;
const INVALID_SPINE_TREE_SNAPSHOT_LABEL: &str = "invalid Spine tree snapshot";

pub(crate) fn new_spine_tree_update(
    turn_id: String,
    snapshot: SpineTreeUpdatedNotification,
) -> SpineTreeUpdateCell {
    SpineTreeUpdateCell {
        turn_id,
        snapshot,
        live: true,
        display_mode: SpineTreeDisplayMode::Pretty,
    }
}

pub(crate) fn new_spine_tree_snapshot(
    snapshot: SpineTreeUpdatedNotification,
) -> SpineTreeUpdateCell {
    SpineTreeUpdateCell {
        turn_id: snapshot.turn_id.clone(),
        snapshot,
        live: false,
        display_mode: SpineTreeDisplayMode::Pretty,
    }
}

pub(crate) fn new_debug_spine_tree_snapshot(
    snapshot: SpineTreeUpdatedNotification,
) -> SpineTreeUpdateCell {
    SpineTreeUpdateCell {
        turn_id: snapshot.turn_id.clone(),
        snapshot,
        live: false,
        display_mode: SpineTreeDisplayMode::Debug,
    }
}

#[derive(Debug)]
pub(crate) struct SpineTreeUpdateCell {
    turn_id: String,
    snapshot: SpineTreeUpdatedNotification,
    live: bool,
    display_mode: SpineTreeDisplayMode,
}

#[derive(Debug, Clone, Copy)]
enum SpineTreeDisplayMode {
    Pretty,
    Debug,
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
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        match self.display_mode {
            SpineTreeDisplayMode::Pretty => pretty_display_lines(&self.snapshot, width),
            SpineTreeDisplayMode::Debug => debug::display_lines(&self.snapshot, width),
        }
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        match self.display_mode {
            SpineTreeDisplayMode::Pretty => pretty_raw_lines(&self.snapshot),
            SpineTreeDisplayMode::Debug => debug::raw_lines(&self.snapshot),
        }
    }
}

fn pretty_display_lines(snapshot: &SpineTreeUpdatedNotification, width: u16) -> Vec<Line<'static>> {
    let mut lines = vec![pretty_header(snapshot)];
    if let Err(error) = validate_spine_tree_snapshot(snapshot) {
        lines.push(invalid_snapshot_display_line(error));
        return lines;
    }

    let root_nodes = child_nodes(snapshot, None);
    if root_nodes.is_empty() {
        lines.push(
            vec![
                format!("  {}", pretty_branch(true)).dim(),
                "(empty)".dim().italic(),
            ]
            .into(),
        );
        return lines;
    }

    let active_path = active_path_ids(snapshot);
    render_pretty_nodes(snapshot, &root_nodes, &active_path, "  ", width, &mut lines);
    lines
}
fn pretty_raw_lines(snapshot: &SpineTreeUpdatedNotification) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from("Spine Tree")];
    if let Err(error) = validate_spine_tree_snapshot(snapshot) {
        lines.push(invalid_snapshot_raw_line(error));
        return lines;
    }

    let root_nodes = child_nodes(snapshot, None);
    if root_nodes.is_empty() {
        lines.push(Line::from(format!("  {}(empty)", pretty_branch(true))));
        return lines;
    }

    let active_path = active_path_ids(snapshot);
    append_pretty_raw_nodes(snapshot, &root_nodes, &active_path, "  ", &mut lines);
    lines
}

fn pretty_header(_snapshot: &SpineTreeUpdatedNotification) -> Line<'static> {
    vec!["• ".dim(), "Spine Tree".bold()].into()
}
fn render_pretty_node(
    snapshot: &SpineTreeUpdatedNotification,
    node: &SpineTreeNode,
    active_path: &HashSet<&str>,
    prefix: &str,
    is_last: bool,
    width: u16,
    out: &mut Vec<Line<'static>>,
) {
    let children = child_nodes(snapshot, Some(node.node_id.as_str()));
    let active = node.node_id == snapshot.active_node_id;
    let line_prefix = format!("{}{}", prefix, pretty_branch(is_last));
    let child_prefix = format!("{}{}", prefix, pretty_child_prefix(is_last));
    let mut spans = vec![
        Span::from(line_prefix).dim(),
        pretty_marker(node, active, !children.is_empty()),
        Span::from(" "),
    ];
    spans.push(Span::from(pretty_node_label_text(node, active)));

    let line = Line::from(spans);
    let wrapped = adaptive_wrap_line(
        &line,
        RtOptions::new(width.saturating_sub(2).max(1) as usize)
            .subsequent_indent(format!("{child_prefix}  ").into()),
    );
    push_owned_lines(&wrapped, out);

    render_pretty_nodes(snapshot, &children, active_path, &child_prefix, width, out);
}

fn render_pretty_nodes(
    snapshot: &SpineTreeUpdatedNotification,
    nodes: &[&SpineTreeNode],
    active_path: &HashSet<&str>,
    prefix: &str,
    width: u16,
    out: &mut Vec<Line<'static>>,
) {
    let items = pretty_render_items(snapshot, nodes, active_path);
    let item_count = items.len();
    for (index, item) in items.into_iter().enumerate() {
        let is_last = index + 1 == item_count;
        match item {
            PrettySiblingItem::HistoryBucket(count) => {
                render_history_bucket(count, prefix, is_last, width, out);
            }
            PrettySiblingItem::Node(node) => {
                render_pretty_node(snapshot, node, active_path, prefix, is_last, width, out);
            }
        }
    }
}

fn append_pretty_raw_nodes(
    snapshot: &SpineTreeUpdatedNotification,
    nodes: &[&SpineTreeNode],
    active_path: &HashSet<&str>,
    prefix: &str,
    out: &mut Vec<Line<'static>>,
) {
    let items = pretty_render_items(snapshot, nodes, active_path);
    let item_count = items.len();
    for (index, item) in items.into_iter().enumerate() {
        let is_last = index + 1 == item_count;
        match item {
            PrettySiblingItem::HistoryBucket(count) => out.push(Line::from(format!(
                "{}{}◌ {}",
                prefix,
                pretty_branch(is_last),
                history_bucket_label(count)
            ))),
            PrettySiblingItem::Node(node) => {
                let children = child_nodes(snapshot, Some(node.node_id.as_str()));
                let active = node.node_id == snapshot.active_node_id;
                let marker = pretty_marker_text(node, active, !children.is_empty());
                out.push(Line::from(format!(
                    "{}{}{} {}",
                    prefix,
                    pretty_branch(is_last),
                    marker,
                    pretty_node_label_text(node, active)
                )));
                let child_prefix = format!("{}{}", prefix, pretty_child_prefix(is_last));
                append_pretty_raw_nodes(snapshot, &children, active_path, &child_prefix, out);
            }
        }
    }
}

fn pretty_render_items<'a>(
    snapshot: &'a SpineTreeUpdatedNotification,
    nodes: &[&'a SpineTreeNode],
    active_path: &HashSet<&str>,
) -> Vec<PrettySiblingItem<'a>> {
    let mut normalized_nodes = Vec::new();
    append_visible_pretty_nodes(snapshot, nodes, &mut normalized_nodes);
    pretty_sibling_items(&normalized_nodes, active_path)
}

fn append_visible_pretty_nodes<'a>(
    snapshot: &'a SpineTreeUpdatedNotification,
    nodes: &[&'a SpineTreeNode],
    out: &mut Vec<&'a SpineTreeNode>,
) {
    for node in nodes.iter().copied() {
        let children = child_nodes(snapshot, Some(node.node_id.as_str()));
        let active = node.node_id == snapshot.active_node_id;
        if should_elide_pretty_node(node, !children.is_empty(), active) {
            append_visible_pretty_nodes(snapshot, &children, out);
        } else {
            out.push(node);
        }
    }
}

enum PrettySiblingItem<'a> {
    HistoryBucket(usize),
    Node(&'a SpineTreeNode),
}

fn pretty_sibling_items<'a>(
    nodes: &[&'a SpineTreeNode],
    active_path: &HashSet<&str>,
) -> Vec<PrettySiblingItem<'a>> {
    let mut items = nodes
        .iter()
        .copied()
        .map(|node| {
            if bucketable_history_node(node, active_path) {
                PrettySiblingItem::HistoryBucket(1)
            } else {
                PrettySiblingItem::Node(node)
            }
        })
        .collect::<Vec<_>>();

    let active_index = nodes
        .iter()
        .position(|node| active_path.contains(node.node_id.as_str()));
    let visible_end = active_index.map_or(nodes.len(), |index| index + 1);
    if visible_end < nodes.len() {
        return merge_adjacent_history_buckets(items);
    };
    if nodes.len() <= PRETTY_MAX_VISIBLE_SIBLINGS {
        return merge_adjacent_history_buckets(items);
    }
    let visible_start = visible_end.saturating_sub(PRETTY_MAX_VISIBLE_SIBLINGS);

    let mut folded = Vec::new();
    if visible_start > 0 {
        let hidden_count = items[..visible_start]
            .iter()
            .map(pretty_sibling_item_history_count)
            .sum();
        folded.push(PrettySiblingItem::HistoryBucket(hidden_count));
    }
    folded.extend(items.drain(visible_start..visible_end));
    merge_adjacent_history_buckets(folded)
}

fn bucketable_history_node(node: &SpineTreeNode, active_path: &HashSet<&str>) -> bool {
    matches!(
        node.status,
        SpineTreeNodeStatus::Closed | SpineTreeNodeStatus::Compacted
    ) && trimmed_summary(node).is_none()
        && !active_path.contains(node.node_id.as_str())
}

fn pretty_sibling_item_history_count(item: &PrettySiblingItem<'_>) -> usize {
    match item {
        PrettySiblingItem::HistoryBucket(count) => *count,
        PrettySiblingItem::Node(_) => 1,
    }
}

fn merge_adjacent_history_buckets<'a>(
    items: Vec<PrettySiblingItem<'a>>,
) -> Vec<PrettySiblingItem<'a>> {
    let mut merged = Vec::with_capacity(items.len());
    for item in items {
        match item {
            PrettySiblingItem::HistoryBucket(count) => {
                if let Some(PrettySiblingItem::HistoryBucket(previous)) = merged.last_mut() {
                    *previous += count;
                } else {
                    merged.push(PrettySiblingItem::HistoryBucket(count));
                }
            }
            PrettySiblingItem::Node(node) => merged.push(PrettySiblingItem::Node(node)),
        }
    }
    merged
}

fn active_path_ids(snapshot: &SpineTreeUpdatedNotification) -> HashSet<&str> {
    let mut active_path = HashSet::new();
    let mut current = snapshot.active_node_id.as_str();
    active_path.insert(current);

    while let Some(node) = snapshot.nodes.iter().find(|node| node.node_id == current) {
        let Some(parent_id) = node.parent_id.as_deref() else {
            break;
        };
        if !active_path.insert(parent_id) {
            break;
        }
        current = parent_id;
    }

    active_path
}

fn pretty_marker(node: &SpineTreeNode, active: bool, has_children: bool) -> Span<'static> {
    match pretty_marker_text(node, active, has_children) {
        "◉" => "◉".cyan().bold(),
        "✓" => "✓".green().bold(),
        "▾" => "▾".dim(),
        "◌" => "◌".yellow().bold(),
        marker => Span::from(marker),
    }
}

fn pretty_marker_text(node: &SpineTreeNode, active: bool, has_children: bool) -> &'static str {
    if active {
        return "◉";
    }
    match node.status {
        SpineTreeNodeStatus::Live => "◉",
        SpineTreeNodeStatus::Closed => "✓",
        SpineTreeNodeStatus::Compacted => "◌",
        SpineTreeNodeStatus::Opened if has_children => "▾",
        SpineTreeNodeStatus::Opened => "◌",
    }
}

fn render_history_bucket(
    count: usize,
    prefix: &str,
    is_last: bool,
    width: u16,
    out: &mut Vec<Line<'static>>,
) {
    let line_prefix = format!("{}{}", prefix, pretty_branch(is_last));
    let child_prefix = format!("{}{}", prefix, pretty_child_prefix(is_last));
    let line = Line::from(vec![
        Span::from(line_prefix).dim(),
        "◌".yellow().bold(),
        " ".into(),
        Span::from(history_bucket_label(count)),
    ]);
    let wrapped = adaptive_wrap_line(
        &line,
        RtOptions::new(width.saturating_sub(2).max(1) as usize)
            .subsequent_indent(format!("{child_prefix}  ").into()),
    );
    push_owned_lines(&wrapped, out);
}

fn history_bucket_label(count: usize) -> String {
    if count == 1 {
        "1 previous task".to_string()
    } else {
        format!("{count} previous tasks")
    }
}

fn pretty_node_label_text(node: &SpineTreeNode, active: bool) -> String {
    trimmed_summary(node)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| pretty_default_node_label(node, active).to_string())
}

fn trimmed_summary(node: &SpineTreeNode) -> Option<&str> {
    node.summary
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

fn should_elide_pretty_node(node: &SpineTreeNode, has_children: bool, active: bool) -> bool {
    has_children && !active && trimmed_summary(node).is_none()
}

fn pretty_default_node_label(node: &SpineTreeNode, active: bool) -> &'static str {
    if active || node.status == SpineTreeNodeStatus::Live {
        return "Current task";
    }
    match node.status {
        SpineTreeNodeStatus::Live => "Current task",
        SpineTreeNodeStatus::Opened => "Task",
        SpineTreeNodeStatus::Closed => "Completed task",
        SpineTreeNodeStatus::Compacted => "Previous task",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpineTreeSnapshotValidationError {
    DuplicateNodeId,
    MissingActiveNode,
    MissingParent,
    ParentCycle,
}

impl SpineTreeSnapshotValidationError {
    fn label(self) -> &'static str {
        match self {
            SpineTreeSnapshotValidationError::DuplicateNodeId => "duplicate node id",
            SpineTreeSnapshotValidationError::MissingActiveNode => "missing active node",
            SpineTreeSnapshotValidationError::MissingParent => "missing parent node",
            SpineTreeSnapshotValidationError::ParentCycle => "parent cycle",
        }
    }
}

fn validate_spine_tree_snapshot(
    snapshot: &SpineTreeUpdatedNotification,
) -> Result<(), SpineTreeSnapshotValidationError> {
    if snapshot.nodes.is_empty() {
        return Ok(());
    }

    let mut node_ids = HashSet::new();
    for node in &snapshot.nodes {
        if !node_ids.insert(node.node_id.as_str()) {
            return Err(SpineTreeSnapshotValidationError::DuplicateNodeId);
        }
    }

    if !node_ids.contains(snapshot.active_node_id.as_str()) {
        return Err(SpineTreeSnapshotValidationError::MissingActiveNode);
    }

    for node in &snapshot.nodes {
        if let Some(parent_id) = node.parent_id.as_deref()
            && !node_ids.contains(parent_id)
        {
            return Err(SpineTreeSnapshotValidationError::MissingParent);
        }
    }

    for node in &snapshot.nodes {
        let mut seen = HashSet::new();
        let mut current_id = Some(node.node_id.as_str());
        while let Some(node_id) = current_id {
            if !seen.insert(node_id) {
                return Err(SpineTreeSnapshotValidationError::ParentCycle);
            }
            current_id = snapshot
                .nodes
                .iter()
                .find(|candidate| candidate.node_id == node_id)
                .and_then(|candidate| candidate.parent_id.as_deref());
        }
    }

    Ok(())
}

fn invalid_snapshot_display_line(error: SpineTreeSnapshotValidationError) -> Line<'static> {
    vec![
        format!("  {}", pretty_branch(true)).dim(),
        Span::from(invalid_snapshot_message(error)).yellow().bold(),
    ]
    .into()
}

fn invalid_snapshot_raw_line(error: SpineTreeSnapshotValidationError) -> Line<'static> {
    Line::from(format!(
        "  {}{}",
        pretty_branch(true),
        invalid_snapshot_message(error)
    ))
}

fn invalid_snapshot_message(error: SpineTreeSnapshotValidationError) -> String {
    format!("{INVALID_SPINE_TREE_SNAPSHOT_LABEL}: {}", error.label())
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

fn pretty_branch(is_last: bool) -> &'static str {
    if is_last { "└ " } else { "├ " }
}

fn pretty_child_prefix(is_last: bool) -> &'static str {
    if is_last { "  " } else { "│ " }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_app_server_protocol::SpineTreeNodeKind;

    fn render(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn snapshot(active_node_id: &str, nodes: Vec<SpineTreeNode>) -> SpineTreeUpdatedNotification {
        SpineTreeUpdatedNotification {
            thread_id: "thread".to_string(),
            turn_id: "turn".to_string(),
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
            kind: SpineTreeNodeKind::Task,
            status,
            summary: summary.map(str::to_string),
            memory_summary: None,
            start: 0,
            end: None,
        }
    }

    #[test]
    fn renders_pretty_hierarchy_and_active_path() {
        let cell = new_spine_tree_snapshot(snapshot(
            "2.1",
            vec![
                node("1", None, Some("earlier work"), SpineTreeNodeStatus::Closed),
                node(
                    "2",
                    None,
                    Some("current scope"),
                    SpineTreeNodeStatus::Opened,
                ),
                node(
                    "2.1",
                    Some("2"),
                    Some("focused task"),
                    SpineTreeNodeStatus::Live,
                ),
            ],
        ));

        insta::assert_snapshot!(render(&cell.display_lines(80)), @r###"
        • Spine Tree
          ├ ✓ earlier work
          └ ▾ current scope
            └ ◉ focused task
        "###);
    }

    #[test]
    fn folds_older_siblings_and_elides_empty_structural_nodes() {
        let cell = new_spine_tree_snapshot(snapshot(
            "3.3",
            vec![
                node("1", None, Some("old root 1"), SpineTreeNodeStatus::Closed),
                node("2", None, Some("old root 2"), SpineTreeNodeStatus::Closed),
                node("3", None, None, SpineTreeNodeStatus::Opened),
                node(
                    "3.1",
                    Some("3"),
                    Some("child 1"),
                    SpineTreeNodeStatus::Closed,
                ),
                node(
                    "3.2",
                    Some("3"),
                    Some("child 2"),
                    SpineTreeNodeStatus::Closed,
                ),
                node(
                    "3.3",
                    Some("3"),
                    Some("active child"),
                    SpineTreeNodeStatus::Live,
                ),
            ],
        ));

        let rendered = render(&cell.display_lines(80));
        insta::assert_snapshot!(rendered, @r###"
        • Spine Tree
          ├ ◌ 2 previous tasks
          ├ ✓ child 1
          ├ ✓ child 2
          └ ◉ active child
        "###);
        assert!(!rendered.contains("old root"));
        assert!(!rendered.contains("3 "));
    }

    #[test]
    fn wraps_long_summary_using_tree_indent() {
        let cell = new_spine_tree_snapshot(snapshot(
            "1",
            vec![node(
                "1",
                None,
                Some("a summary that is deliberately long enough to wrap"),
                SpineTreeNodeStatus::Live,
            )],
        ));

        let lines = cell.display_lines(24);
        assert!(lines.len() > 2);
        assert!(render(&lines).contains("  └ ◉ "));
    }

    #[test]
    fn reports_invalid_parent_snapshot_without_panicking() {
        let cell = new_spine_tree_snapshot(snapshot(
            "1",
            vec![SpineTreeNode {
                node_id: "1".to_string(),
                parent_id: Some("missing".to_string()),
                kind: SpineTreeNodeKind::Task,
                status: SpineTreeNodeStatus::Live,
                summary: None,
                memory_summary: None,
                start: 0,
                end: None,
            }],
        ));

        assert!(
            render(&cell.display_lines(80))
                .contains("invalid Spine tree snapshot: missing parent node")
        );
    }
}
