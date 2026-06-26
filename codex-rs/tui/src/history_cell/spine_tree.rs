//! Host-only Spine tree projection history cell.

use super::*;
use codex_app_server_protocol::SpineTreeNode;
use codex_app_server_protocol::SpineTreeNodeStatus;
use codex_app_server_protocol::SpineTreeUpdatedNotification;
#[cfg(debug_assertions)]
use codex_protocol::num_format::format_si_suffix;
use std::collections::HashSet;

const PRETTY_MAX_VISIBLE_SIBLINGS: usize = 3;
const INVALID_SPINE_TREE_SNAPSHOT_LABEL: &str = "invalid Spine tree snapshot";

pub(crate) fn new_spine_tree_update(
    turn_id: String,
    snapshot: SpineTreeUpdatedNotification,
) -> SpineTreeUpdateCell {
    SpineTreeUpdateCell {
        turn_id,
        snapshot,
        source: SpineTreeUpdateSource::Live,
        display_mode: SpineTreeDisplayMode::Pretty,
    }
}

pub(crate) fn new_manual_spine_tree_snapshot(
    snapshot: SpineTreeUpdatedNotification,
) -> SpineTreeUpdateCell {
    SpineTreeUpdateCell {
        turn_id: snapshot.turn_id.clone(),
        snapshot,
        source: SpineTreeUpdateSource::Manual,
        display_mode: SpineTreeDisplayMode::Pretty,
    }
}

#[cfg(debug_assertions)]
pub(crate) fn new_manual_debug_spine_tree_snapshot(
    snapshot: SpineTreeUpdatedNotification,
) -> SpineTreeUpdateCell {
    SpineTreeUpdateCell {
        turn_id: snapshot.turn_id.clone(),
        snapshot,
        source: SpineTreeUpdateSource::Manual,
        display_mode: SpineTreeDisplayMode::Debug,
    }
}

#[derive(Debug)]
pub(crate) struct SpineTreeUpdateCell {
    turn_id: String,
    snapshot: SpineTreeUpdatedNotification,
    source: SpineTreeUpdateSource,
    display_mode: SpineTreeDisplayMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpineTreeUpdateSource {
    Live,
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpineTreeDisplayMode {
    Pretty,
    #[cfg(debug_assertions)]
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
        self.source == SpineTreeUpdateSource::Live
    }
}

impl HistoryCell for SpineTreeUpdateCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        match self.display_mode {
            SpineTreeDisplayMode::Pretty => pretty_display_lines(&self.snapshot, width),
            #[cfg(debug_assertions)]
            SpineTreeDisplayMode::Debug => debug_display_lines(&self.snapshot, width),
        }
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        match self.display_mode {
            SpineTreeDisplayMode::Pretty => pretty_raw_lines(&self.snapshot),
            #[cfg(debug_assertions)]
            SpineTreeDisplayMode::Debug => debug_raw_lines(&self.snapshot),
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

#[cfg(debug_assertions)]
fn debug_display_lines(snapshot: &SpineTreeUpdatedNotification, width: u16) -> Vec<Line<'static>> {
    let mut lines = vec![debug_header(snapshot)];
    if let Err(error) = validate_spine_tree_snapshot(snapshot) {
        lines.push(invalid_snapshot_display_line(error));
        return lines;
    }

    let root_nodes = child_nodes(snapshot, None);
    if root_nodes.is_empty() {
        lines.push(vec!["  └ ".dim(), "(empty)".dim().italic()].into());
        return lines;
    }

    let root_count = root_nodes.len();
    for (index, node) in root_nodes.into_iter().enumerate() {
        render_debug_node(
            snapshot,
            node,
            0,
            index + 1 == root_count,
            width,
            &mut lines,
        );
    }
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

#[cfg(debug_assertions)]
fn debug_raw_lines(snapshot: &SpineTreeUpdatedNotification) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(format!(
        "Debug Spine Tree current {}",
        snapshot.active_node_id
    ))];
    if let Err(error) = validate_spine_tree_snapshot(snapshot) {
        lines.push(Line::from(invalid_snapshot_message(error)));
        return lines;
    }
    append_debug_raw_children(snapshot, None, 0, &mut lines);
    lines
}

fn pretty_header(_snapshot: &SpineTreeUpdatedNotification) -> Line<'static> {
    vec!["• ".dim(), "Spine Tree".bold()].into()
}

#[cfg(debug_assertions)]
fn debug_header(snapshot: &SpineTreeUpdatedNotification) -> Line<'static> {
    vec![
        "• ".dim(),
        "Debug Spine Tree".bold(),
        "  ".dim(),
        "current ".dim(),
        Span::from(snapshot.active_node_id.clone()).cyan().bold(),
    ]
    .into()
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

#[cfg(debug_assertions)]
fn render_debug_node(
    snapshot: &SpineTreeUpdatedNotification,
    node: &SpineTreeNode,
    depth: usize,
    is_last: bool,
    width: u16,
    out: &mut Vec<Line<'static>>,
) {
    let active = node.node_id == snapshot.active_node_id;
    let status = debug_node_status_label(node.status, active);
    let line_prefix = format!("  {}{}", "  ".repeat(depth), pretty_branch(is_last));
    let mut spans = vec![
        Span::from(line_prefix).dim(),
        Span::from(node.node_id.clone()).cyan().bold(),
    ];
    if let Some(summary) = trimmed_summary(node) {
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
    if let Some(accounting) = format_node_accounting(node) {
        spans.push(Span::from(" "));
        spans.push(Span::from(accounting).dim());
    }

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
        render_debug_node(
            snapshot,
            child,
            depth + 1,
            index + 1 == child_count,
            width,
            out,
        );
    }
}

#[cfg(debug_assertions)]
fn append_debug_raw_children(
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
        let summary = trimmed_summary(node)
            .map(|summary| format!(" {summary}"))
            .unwrap_or_default();
        let active = node.node_id == snapshot.active_node_id;
        let accounting = format_node_accounting(node)
            .map(|accounting| format!(" {accounting}"))
            .unwrap_or_default();
        out.push(Line::from(format!(
            "{}{}{}{} {}{}",
            "  ".repeat(depth),
            marker,
            node.node_id,
            summary,
            debug_node_status_label(node.status, active),
            accounting
        )));
        append_debug_raw_children(snapshot, Some(node.node_id.as_str()), depth + 1, out);
    }
}

#[cfg(debug_assertions)]
fn format_node_accounting(node: &SpineTreeNode) -> Option<String> {
    let accounting = node.accounting.as_ref()?;
    if matches!(
        node.status,
        SpineTreeNodeStatus::Live | SpineTreeNodeStatus::Opened
    ) {
        if let Some(tokens) = non_negative_tokens(accounting.current_node_context_tokens) {
            return Some(format!("(~{} inclusive context)", format_si_suffix(tokens)));
        }
        if let Some(problem) = accounting.current_node_context_problem {
            return Some(format!(
                "(context problem: {})",
                context_problem_label(problem)
            ));
        }
    }
    match (
        non_negative_tokens(accounting.closed_source_suffix_tokens),
        non_negative_tokens(accounting.closed_memory_context_tokens),
        positive_tokens(accounting.memory_output_tokens),
    ) {
        (Some(source), Some(memory), _) => Some(format!(
            "(~{} source -> ~{} memory context)",
            format_si_suffix(source),
            format_si_suffix(memory)
        )),
        (Some(source), None, Some(output)) => Some(format!(
            "(~{} source -> ~{} memory output)",
            format_si_suffix(source),
            format_si_suffix(output)
        )),
        (Some(source), None, None) => Some(format!("(~{} source)", format_si_suffix(source))),
        (None, Some(memory), _) => Some(format!("(~{} memory context)", format_si_suffix(memory))),
        (None, None, Some(output)) => {
            Some(format!("(~{} memory output)", format_si_suffix(output)))
        }
        (None, None, None) => None,
    }
}

#[cfg(debug_assertions)]
fn positive_tokens(tokens: Option<i64>) -> Option<i64> {
    tokens.filter(|tokens| *tokens > 0)
}

#[cfg(debug_assertions)]
fn non_negative_tokens(tokens: Option<i64>) -> Option<i64> {
    tokens.filter(|tokens| *tokens >= 0)
}

#[cfg(debug_assertions)]
fn context_problem_label(
    problem: codex_app_server_protocol::SpineNodeContextProblem,
) -> &'static str {
    match problem {
        codex_app_server_protocol::SpineNodeContextProblem::MissingCurrentUsage => {
            "missing current usage"
        }
        codex_app_server_protocol::SpineNodeContextProblem::MissingOpenContextBaseline => {
            "missing open baseline"
        }
        codex_app_server_protocol::SpineNodeContextProblem::CoordinateMismatch => {
            "coordinate mismatch"
        }
        codex_app_server_protocol::SpineNodeContextProblem::CorruptPressureMetadata => {
            "corrupt pressure metadata"
        }
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

fn pretty_branch(is_last: bool) -> &'static str {
    if is_last { "└ " } else { "├ " }
}

fn pretty_child_prefix(is_last: bool) -> &'static str {
    if is_last { "  " } else { "│ " }
}

#[cfg(debug_assertions)]
fn debug_node_status_label(status: SpineTreeNodeStatus, active: bool) -> &'static str {
    if active {
        return "current";
    }
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
    use codex_app_server_protocol::SpineTreeNodeAccounting;

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
            accounting: None,
        }
    }

    fn accounting(
        current_node_context_tokens: Option<i64>,
        closed_source_suffix_tokens: Option<i64>,
        closed_memory_context_tokens: Option<i64>,
        memory_output_tokens: Option<i64>,
    ) -> Option<SpineTreeNodeAccounting> {
        Some(SpineTreeNodeAccounting {
            current_node_context_tokens,
            current_node_context_problem: None,
            current_node_context_baseline_source: None,
            closed_source_suffix_tokens,
            closed_memory_context_tokens,
            memory_output_tokens,
        })
    }

    #[cfg(debug_assertions)]
    fn problem_accounting(
        problem: codex_app_server_protocol::SpineNodeContextProblem,
    ) -> Option<SpineTreeNodeAccounting> {
        Some(SpineTreeNodeAccounting {
            current_node_context_tokens: None,
            current_node_context_problem: Some(problem),
            current_node_context_baseline_source: None,
            closed_source_suffix_tokens: None,
            closed_memory_context_tokens: None,
            memory_output_tokens: None,
        })
    }

    #[test]
    fn renders_visible_tree_without_internal_terms() {
        let cell = new_spine_tree_update(
            "turn".to_string(),
            snapshot(vec![
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
            ]),
        );

        let rendered = render_lines(&cell.display_lines(80)).join("\n");
        insta::assert_snapshot!(rendered, @r###"
        • Spine Tree
          ├ ✓ earlier work
          └ ▾ current scope
            └ ◉ focused task
        "###);
        assert!(rendered.contains("Spine Tree"));
        assert!(!rendered.contains("1 earlier work"));
        assert!(!rendered.contains("2 current scope"));
        assert!(!rendered.contains("2.1 focused task"));
        assert!(!rendered.contains("done"));
        assert!(!rendered.contains("open"));
        assert!(!rendered.contains("LR"));
        assert!(!rendered.contains("ParseStack"));
        assert!(!rendered.contains("raw"));
        assert!(!rendered.contains("memory="));
        assert!(!rendered.contains("trajs="));
        assert!(!rendered.contains("PlanTree"));
    }

    #[test]
    fn pretty_uses_green_check_for_closed_nodes() {
        let cell = new_spine_tree_update(
            "turn".to_string(),
            snapshot_with_active(
                "2",
                vec![
                    node("1", None, Some("finished"), SpineTreeNodeStatus::Closed),
                    node("2", None, Some("active"), SpineTreeNodeStatus::Live),
                ],
            ),
        );

        let lines = cell.display_lines(80);
        let check_span = lines[1]
            .spans
            .iter()
            .find(|span| span.content.as_ref() == "✓")
            .expect("closed node should render a check mark");
        assert_eq!(check_span.style.fg, Some(Color::Green));
    }

    #[test]
    fn pretty_uses_non_success_marker_for_compacted_nodes() {
        let cell = new_spine_tree_update(
            "turn".to_string(),
            snapshot_with_active(
                "2",
                vec![
                    node("1", None, Some("compacted"), SpineTreeNodeStatus::Compacted),
                    node("2", None, Some("active"), SpineTreeNodeStatus::Live),
                ],
            ),
        );

        let lines = cell.display_lines(80);
        let marker_span = lines[1]
            .spans
            .iter()
            .find(|span| span.content.as_ref() == "◌")
            .expect("compacted node should render a non-success marker");
        assert_eq!(marker_span.style.fg, Some(Color::Yellow));
    }

    #[test]
    fn pretty_renders_empty_snapshot_in_display_and_raw() {
        let cell = new_spine_tree_update(
            "turn".to_string(),
            snapshot_with_active("missing", Vec::new()),
        );

        let rendered = render_lines(&cell.display_lines(80)).join("\n");
        insta::assert_snapshot!(rendered, @r###"
        • Spine Tree
          └ (empty)
        "###);

        let raw = render_lines(&cell.raw_lines()).join("\n");
        insta::assert_snapshot!(raw, @r###"
        Spine Tree
          └ (empty)
        "###);
    }

    #[test]
    fn pretty_rejects_invalid_snapshot_in_display_and_raw() {
        let cell = new_spine_tree_update(
            "turn".to_string(),
            snapshot_with_active(
                "missing",
                vec![node(
                    "1",
                    None,
                    Some("unreachable task"),
                    SpineTreeNodeStatus::Closed,
                )],
            ),
        );

        let rendered = render_lines(&cell.display_lines(80)).join("\n");
        insta::assert_snapshot!(rendered, @r###"
        • Spine Tree
          └ invalid Spine tree snapshot: missing active node
        "###);
        assert!(!rendered.contains("unreachable task"));

        let raw = render_lines(&cell.raw_lines()).join("\n");
        insta::assert_snapshot!(raw, @r###"
        Spine Tree
          └ invalid Spine tree snapshot: missing active node
        "###);
        assert!(!raw.contains("unreachable task"));
    }

    #[test]
    fn rejects_malformed_snapshot_shapes() {
        assert_eq!(
            validate_spine_tree_snapshot(&snapshot_with_active(
                "1",
                vec![
                    node("1", None, Some("first"), SpineTreeNodeStatus::Live),
                    node("1", None, Some("duplicate"), SpineTreeNodeStatus::Closed),
                ],
            )),
            Err(SpineTreeSnapshotValidationError::DuplicateNodeId)
        );
        assert_eq!(
            validate_spine_tree_snapshot(&snapshot_with_active(
                "missing",
                vec![node("1", None, Some("first"), SpineTreeNodeStatus::Closed)],
            )),
            Err(SpineTreeSnapshotValidationError::MissingActiveNode)
        );
        assert_eq!(
            validate_spine_tree_snapshot(&snapshot_with_active(
                "1",
                vec![node(
                    "1",
                    Some("missing-parent"),
                    Some("first"),
                    SpineTreeNodeStatus::Live,
                )],
            )),
            Err(SpineTreeSnapshotValidationError::MissingParent)
        );
        assert_eq!(
            validate_spine_tree_snapshot(&snapshot_with_active(
                "1",
                vec![
                    node("1", Some("2"), Some("first"), SpineTreeNodeStatus::Live),
                    node("2", Some("1"), Some("second"), SpineTreeNodeStatus::Opened),
                ],
            )),
            Err(SpineTreeSnapshotValidationError::ParentCycle)
        );
    }

    #[test]
    fn pretty_labels_single_blank_root_compact_as_previous_task() {
        let cell = new_spine_tree_update(
            "turn".to_string(),
            snapshot_with_active(
                "2.1",
                vec![
                    node("1", None, None, SpineTreeNodeStatus::Compacted),
                    node("2", None, None, SpineTreeNodeStatus::Opened),
                    node("2.1", Some("2"), Some("active"), SpineTreeNodeStatus::Live),
                ],
            ),
        );

        let rendered = render_lines(&cell.display_lines(80)).join("\n");
        insta::assert_snapshot!(rendered, @r###"
        • Spine Tree
          ├ ◌ 1 previous task
          └ ◉ active
        "###);
        assert!(!rendered.contains("Previous context"));
        assert!(!rendered.contains("Previous task"));
    }

    #[test]
    fn pretty_merges_folded_history_with_adjacent_blank_history() {
        let cell = new_spine_tree_update(
            "turn".to_string(),
            snapshot_with_active(
                "4",
                vec![
                    node("1", None, Some("older"), SpineTreeNodeStatus::Closed),
                    node("2", None, None, SpineTreeNodeStatus::Compacted),
                    node("3", None, Some("recent"), SpineTreeNodeStatus::Closed),
                    node("4", None, Some("active"), SpineTreeNodeStatus::Live),
                ],
            ),
        );

        let rendered = render_lines(&cell.display_lines(80)).join("\n");
        insta::assert_snapshot!(rendered, @r###"
        • Spine Tree
          ├ ◌ 2 previous tasks
          ├ ✓ recent
          └ ◉ active
        "###);
        assert!(!rendered.contains("older"));
        assert!(!rendered.contains("Previous task"));
        assert!(!rendered.contains("Completed task"));
    }

    #[test]
    fn pretty_merges_blank_closed_and_compacted_history_under_budget() {
        let cell = new_spine_tree_update(
            "turn".to_string(),
            snapshot_with_active(
                "3",
                vec![
                    node("1", None, None, SpineTreeNodeStatus::Compacted),
                    node("2", None, None, SpineTreeNodeStatus::Closed),
                    node("3", None, Some("active"), SpineTreeNodeStatus::Live),
                ],
            ),
        );

        let rendered = render_lines(&cell.display_lines(80)).join("\n");
        insta::assert_snapshot!(rendered, @r###"
        • Spine Tree
          ├ ◌ 2 previous tasks
          └ ◉ active
        "###);
        assert!(!rendered.contains("Previous task"));
        assert!(!rendered.contains("Completed task"));

        let raw = render_lines(&cell.raw_lines()).join("\n");
        insta::assert_snapshot!(raw, @r###"
        Spine Tree
          ├ ◌ 2 previous tasks
          └ ◉ active
        "###);
    }

    #[test]
    fn pretty_merges_folded_history_before_named_completed_tasks() {
        let cell = new_spine_tree_update(
            "turn".to_string(),
            snapshot_with_active(
                "16",
                vec![
                    node("1", None, Some("old 1"), SpineTreeNodeStatus::Closed),
                    node("2", None, Some("old 2"), SpineTreeNodeStatus::Closed),
                    node("3", None, Some("old 3"), SpineTreeNodeStatus::Closed),
                    node("4", None, Some("old 4"), SpineTreeNodeStatus::Closed),
                    node("5", None, Some("old 5"), SpineTreeNodeStatus::Closed),
                    node("6", None, Some("old 6"), SpineTreeNodeStatus::Closed),
                    node("7", None, Some("old 7"), SpineTreeNodeStatus::Closed),
                    node("8", None, Some("old 8"), SpineTreeNodeStatus::Closed),
                    node("9", None, Some("old 9"), SpineTreeNodeStatus::Closed),
                    node("10", None, Some("old 10"), SpineTreeNodeStatus::Closed),
                    node("11", None, Some("old 11"), SpineTreeNodeStatus::Closed),
                    node("12", None, Some("old 12"), SpineTreeNodeStatus::Closed),
                    node("13", None, None, SpineTreeNodeStatus::Compacted),
                    node(
                        "14",
                        None,
                        Some("Document confirmed true bugs and repair plan"),
                        SpineTreeNodeStatus::Closed,
                    ),
                    node(
                        "15",
                        None,
                        Some("验证 P1-E-1 POC 并复核 P1-D-1 设计意图"),
                        SpineTreeNodeStatus::Closed,
                    ),
                    node(
                        "16",
                        None,
                        Some("更新 P1-E/P1-D 复核结论文档"),
                        SpineTreeNodeStatus::Live,
                    ),
                ],
            ),
        );

        let rendered = render_lines(&cell.display_lines(120)).join("\n");
        insta::assert_snapshot!(rendered, @r###"
        • Spine Tree
          ├ ◌ 13 previous tasks
          ├ ✓ Document confirmed true bugs and repair plan
          ├ ✓ 验证 P1-E-1 POC 并复核 P1-D-1 设计意图
          └ ◉ 更新 P1-E/P1-D 复核结论文档
        "###);
        assert!(!rendered.contains("old 1"));
        assert!(!rendered.contains("Previous task"));
        assert!(rendered.contains("✓ Document confirmed true bugs and repair plan"));
        assert!(rendered.contains("✓ 验证 P1-E-1 POC 并复核 P1-D-1 设计意图"));
    }

    #[test]
    fn pretty_folds_root_siblings_over_budget() {
        let cell = new_spine_tree_update(
            "turn".to_string(),
            snapshot_with_active(
                "6",
                vec![
                    node("1", None, Some("old 1"), SpineTreeNodeStatus::Closed),
                    node("2", None, Some("old 2"), SpineTreeNodeStatus::Closed),
                    node("3", None, Some("old 3"), SpineTreeNodeStatus::Closed),
                    node("4", None, Some("recent 1"), SpineTreeNodeStatus::Closed),
                    node("5", None, Some("recent 2"), SpineTreeNodeStatus::Closed),
                    node("6", None, Some("active work"), SpineTreeNodeStatus::Live),
                ],
            ),
        );

        let rendered = render_lines(&cell.display_lines(80)).join("\n");
        insta::assert_snapshot!(rendered, @r###"
        • Spine Tree
          ├ ◌ 3 previous tasks
          ├ ✓ recent 1
          ├ ✓ recent 2
          └ ◉ active work
        "###);
        assert!(!rendered.contains("old 1"));
        assert!(!rendered.contains("old 2"));
        assert!(!rendered.contains("old 3"));
        assert!(!rendered.contains("previous contexts"));

        let raw = render_lines(&cell.raw_lines()).join("\n");
        insta::assert_snapshot!(raw, @r###"
        Spine Tree
          ├ ◌ 3 previous tasks
          ├ ✓ recent 1
          ├ ✓ recent 2
          └ ◉ active work
        "###);
    }

    #[test]
    fn pretty_folds_nested_siblings_per_parent() {
        let cell = new_spine_tree_update(
            "turn".to_string(),
            snapshot_with_active(
                "2.6",
                vec![
                    node(
                        "2",
                        None,
                        Some("current scope"),
                        SpineTreeNodeStatus::Opened,
                    ),
                    node(
                        "2.1",
                        Some("2"),
                        Some("child 1"),
                        SpineTreeNodeStatus::Closed,
                    ),
                    node(
                        "2.2",
                        Some("2"),
                        Some("child 2"),
                        SpineTreeNodeStatus::Closed,
                    ),
                    node(
                        "2.3",
                        Some("2"),
                        Some("child 3"),
                        SpineTreeNodeStatus::Closed,
                    ),
                    node(
                        "2.4",
                        Some("2"),
                        Some("child 4"),
                        SpineTreeNodeStatus::Closed,
                    ),
                    node(
                        "2.5",
                        Some("2"),
                        Some("child 5"),
                        SpineTreeNodeStatus::Closed,
                    ),
                    node(
                        "2.6",
                        Some("2"),
                        Some("active child"),
                        SpineTreeNodeStatus::Live,
                    ),
                ],
            ),
        );

        let rendered = render_lines(&cell.display_lines(80)).join("\n");
        insta::assert_snapshot!(rendered, @r###"
        • Spine Tree
          └ ▾ current scope
            ├ ◌ 3 previous tasks
            ├ ✓ child 4
            ├ ✓ child 5
            └ ◉ active child
        "###);
        assert!(!rendered.contains("child 1"));
        assert!(!rendered.contains("child 2"));
        assert!(!rendered.contains("child 3"));
        assert!(!rendered.contains("previous contexts"));
    }

    #[test]
    fn pretty_folds_after_eliding_structural_nodes() {
        let cell = new_spine_tree_update(
            "turn".to_string(),
            snapshot_with_active(
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
            ),
        );

        let rendered = render_lines(&cell.display_lines(80)).join("\n");
        insta::assert_snapshot!(rendered, @r###"
        • Spine Tree
          ├ ◌ 2 previous tasks
          ├ ✓ child 1
          ├ ✓ child 2
          └ ◉ active child
        "###);
        assert!(!rendered.contains("old root 1"));
        assert!(!rendered.contains("old root 2"));

        let raw = render_lines(&cell.raw_lines()).join("\n");
        insta::assert_snapshot!(raw, @r###"
        Spine Tree
          ├ ◌ 2 previous tasks
          ├ ✓ child 1
          ├ ✓ child 2
          └ ◉ active child
        "###);
    }

    #[test]
    fn pretty_omits_context_accounting() {
        let mut closed = node("1", None, Some("previous"), SpineTreeNodeStatus::Compacted);
        closed.accounting = accounting(None, Some(7_500), Some(7_500), Some(1_250));
        let mut active = node("2.1", None, Some("active"), SpineTreeNodeStatus::Live);
        active.accounting = accounting(Some(181_546), None, None, None);
        let cell = new_spine_tree_update("turn".to_string(), snapshot(vec![closed, active]));

        let rendered = render_lines(&cell.display_lines(80)).join("\n");
        insta::assert_snapshot!(rendered, @r###"
        • Spine Tree
          ├ ◌ previous
          └ ◉ active
        "###);
        assert!(!rendered.contains("inclusive context"));
        assert!(!rendered.contains("raw"));
        assert!(!rendered.contains("memory"));
        assert!(!rendered.contains("compacted"));

        let raw = render_lines(&cell.raw_lines()).join("\n");
        insta::assert_snapshot!(raw, @r###"
        Spine Tree
          ├ ◌ previous
          └ ◉ active
        "###);
        assert!(!raw.contains("inclusive context"));
        assert!(!raw.contains("raw"));
        assert!(!raw.contains("memory"));
    }

    #[cfg(debug_assertions)]
    #[test]
    fn debug_renders_context_accounting() {
        let mut closed = node("1", None, Some("previous"), SpineTreeNodeStatus::Compacted);
        closed.accounting = accounting(None, Some(7_500), Some(7_500), Some(1_250));
        let mut opened = node("2", None, Some("outer"), SpineTreeNodeStatus::Opened);
        opened.accounting = accounting(Some(231_546), None, None, None);
        let mut active = node("2.1", None, Some("active"), SpineTreeNodeStatus::Live);
        active.accounting = accounting(Some(181_546), None, None, None);
        let cell = new_manual_debug_spine_tree_snapshot(snapshot(vec![closed, opened, active]));

        let rendered = render_lines(&cell.display_lines(80)).join("\n");
        insta::assert_snapshot!(rendered, @r###"
        • Debug Spine Tree  current 2.1
          ├ 1 previous compacted (~7.50K source -> ~7.50K memory context)
          ├ 2 outer open (~232K inclusive context)
          └ 2.1 active current (~182K inclusive context)
        "###);
        assert!(rendered.contains("1 previous compacted (~7.50K source -> ~7.50K memory context)"));
        assert!(rendered.contains("2 outer open (~232K inclusive context)"));
        assert!(rendered.contains("2.1 active current (~182K inclusive context)"));

        let raw = render_lines(&cell.raw_lines()).join("\n");
        insta::assert_snapshot!(raw, @r###"
        Debug Spine Tree current 2.1
        1 previous compacted (~7.50K source -> ~7.50K memory context)
        2 outer open (~232K inclusive context)
        * 2.1 active current (~182K inclusive context)
        "###);
        assert!(raw.contains("1 previous compacted (~7.50K source -> ~7.50K memory context)"));
        assert!(raw.contains("2 outer open (~232K inclusive context)"));
        assert!(raw.contains("* 2.1 active current (~182K inclusive context)"));
    }

    #[cfg(debug_assertions)]
    #[test]
    fn debug_renders_context_problem() {
        let mut opened = node("2", None, Some("outer"), SpineTreeNodeStatus::Opened);
        opened.accounting = problem_accounting(
            codex_app_server_protocol::SpineNodeContextProblem::MissingOpenContextBaseline,
        );
        let mut active = node("2.1", Some("2"), Some("active"), SpineTreeNodeStatus::Live);
        active.accounting = accounting(Some(181_546), None, None, None);
        let cell = new_manual_debug_spine_tree_snapshot(snapshot(vec![opened, active]));

        let rendered = render_lines(&cell.display_lines(80)).join("\n");
        assert!(rendered.contains("2 outer open (context problem: missing open baseline)"));
        assert!(rendered.contains("2.1 active current (~182K inclusive context)"));
    }

    #[cfg(debug_assertions)]
    #[test]
    fn debug_renders_open_context_zero_value() {
        let mut active = node("2.1", None, Some("active"), SpineTreeNodeStatus::Live);
        active.accounting = accounting(Some(0), None, None, None);
        let cell = new_manual_debug_spine_tree_snapshot(snapshot(vec![active]));

        let rendered = render_lines(&cell.display_lines(80)).join("\n");
        assert!(rendered.contains("2.1 active current (~0 inclusive context)"));
        assert!(rendered.contains("~0 inclusive context"));
        assert!(!rendered.contains("raw"));
        assert!(!rendered.contains("memory"));
    }

    #[cfg(debug_assertions)]
    #[test]
    fn debug_omits_absent_context_accounting() {
        let mut active = node("2.1", None, Some("active"), SpineTreeNodeStatus::Live);
        active.accounting = accounting(None, None, None, None);
        let cell = new_manual_debug_spine_tree_snapshot(snapshot(vec![active]));

        let rendered = render_lines(&cell.display_lines(80)).join("\n");
        assert!(rendered.contains("2.1 active current"));
        assert!(!rendered.contains("inclusive context"));
        assert!(!rendered.contains("raw"));
        assert!(!rendered.contains("memory"));
    }

    #[cfg(debug_assertions)]
    #[test]
    fn debug_renders_closed_accounting_zero_values() {
        let mut closed = node("1", None, Some("previous"), SpineTreeNodeStatus::Compacted);
        closed.accounting = accounting(None, Some(0), Some(0), Some(0));
        let active = node("2", None, Some("active"), SpineTreeNodeStatus::Live);
        let cell =
            new_manual_debug_spine_tree_snapshot(snapshot_with_active("2", vec![closed, active]));

        let rendered = render_lines(&cell.display_lines(80)).join("\n");
        assert!(rendered.contains("1 previous compacted (~0 source -> ~0 memory context)"));
        assert!(rendered.contains("~0 source"));
        assert!(rendered.contains("~0 memory context"));
    }

    #[cfg(debug_assertions)]
    #[test]
    fn renders_root_compact_status_distinctly() {
        let cell = new_manual_debug_spine_tree_snapshot(snapshot(vec![
            node("1", None, Some("old scope"), SpineTreeNodeStatus::Compacted),
            node("2", None, Some("new scope"), SpineTreeNodeStatus::Opened),
            node("2.1", Some("2"), Some("active"), SpineTreeNodeStatus::Live),
        ]));

        let rendered = render_lines(&cell.display_lines(80)).join("\n");
        assert!(rendered.contains("1 old scope compacted"));
        assert!(!rendered.contains("1 old scope done"));
    }

    #[cfg(debug_assertions)]
    #[test]
    fn debug_keeps_blank_root_compact_placeholders_expanded() {
        let cell = new_manual_debug_spine_tree_snapshot(snapshot_with_active(
            "3.1",
            vec![
                node("1", None, None, SpineTreeNodeStatus::Compacted),
                node("2", None, None, SpineTreeNodeStatus::Compacted),
                node("3", None, Some("new scope"), SpineTreeNodeStatus::Opened),
                node("3.1", Some("3"), Some("active"), SpineTreeNodeStatus::Live),
            ],
        ));

        let rendered = render_lines(&cell.display_lines(80)).join("\n");
        insta::assert_snapshot!(rendered, @r###"
        • Debug Spine Tree  current 3.1
          ├ 1 compacted
          ├ 2 compacted
          └ 3 new scope open
            └ 3.1 active current
        "###);
        assert!(rendered.contains("1 compacted"));
        assert!(rendered.contains("2 compacted"));
        assert!(!rendered.contains("2 previous contexts"));
        assert!(!rendered.contains("2 previous tasks"));
    }

    #[test]
    fn renders_promoted_snapshot_root_without_empty_placeholder() {
        let cell = new_spine_tree_update(
            "turn".to_string(),
            snapshot_with_active(
                "1.1.1",
                vec![
                    node("1.1", None, None, SpineTreeNodeStatus::Opened),
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
        assert!(!rendered.contains("1.1"));
        assert!(rendered.contains("◉ focused task"));
    }

    #[test]
    fn renders_active_root_cursor_with_closed_child() {
        let cell = new_spine_tree_update(
            "turn".to_string(),
            snapshot_with_active(
                "1",
                vec![
                    node("1", None, None, SpineTreeNodeStatus::Live),
                    node("1.1", Some("1"), None, SpineTreeNodeStatus::Closed),
                ],
            ),
        );

        let rendered = render_lines(&cell.display_lines(80)).join("\n");
        insta::assert_snapshot!(rendered, @r###"
        • Spine Tree
          └ ◉ Current task
            └ ◌ 1 previous task
        "###);
        assert!(!rendered.contains("(empty)"));
        assert!(!rendered.contains("1.1"));
        assert!(rendered.contains("Current task"));
        assert!(rendered.contains("1 previous task"));
        assert!(!rendered.contains("Completed task"));
        assert!(!rendered.contains("root"));

        let raw = render_lines(&cell.raw_lines()).join("\n");
        insta::assert_snapshot!(raw, @r###"
        Spine Tree
          └ ◉ Current task
            └ ◌ 1 previous task
        "###);
    }
}
