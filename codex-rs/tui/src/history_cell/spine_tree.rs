//! Host-only Spine tree projection history cell.

use super::*;
use codex_app_server_protocol::SpineTreeNode;
use codex_app_server_protocol::SpineTreeNodeStatus;
use codex_app_server_protocol::SpineTreeUpdatedNotification;
#[cfg(debug_assertions)]
use codex_protocol::num_format::format_si_suffix;

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
    let root_nodes = child_nodes(snapshot, None);
    if root_nodes.is_empty() {
        lines.push(vec!["  └ ".dim(), "(empty)".dim().italic()].into());
        return lines;
    }

    for node in root_nodes {
        render_pretty_node(snapshot, node, 0, width, &mut lines);
    }
    lines
}

#[cfg(debug_assertions)]
fn debug_display_lines(snapshot: &SpineTreeUpdatedNotification, width: u16) -> Vec<Line<'static>> {
    let mut lines = vec![debug_header(snapshot)];
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
    let mut lines = vec![Line::from(format!(
        "Spine Tree current {}",
        snapshot.active_node_id
    ))];
    append_pretty_raw_children(snapshot, None, 0, &mut lines);
    lines
}

#[cfg(debug_assertions)]
fn debug_raw_lines(snapshot: &SpineTreeUpdatedNotification) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(format!(
        "Debug Spine Tree current {}",
        snapshot.active_node_id
    ))];
    append_debug_raw_children(snapshot, None, 0, &mut lines);
    lines
}

fn pretty_header(snapshot: &SpineTreeUpdatedNotification) -> Line<'static> {
    vec![
        "• ".dim(),
        "Spine Tree".bold(),
        "  ".dim(),
        "current ".dim(),
        Span::from(snapshot.active_node_id.clone()).cyan().bold(),
    ]
    .into()
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
    depth: usize,
    width: u16,
    out: &mut Vec<Line<'static>>,
) {
    let children = child_nodes(snapshot, Some(node.node_id.as_str()));
    let active = node.node_id == snapshot.active_node_id;
    let mut spans = vec![
        Span::from(format!("  {}", "  ".repeat(depth))).dim(),
        pretty_marker(node, active, !children.is_empty()),
        Span::from(" "),
    ];
    spans.push(pretty_node_label(node));

    let line = Line::from(spans);
    let wrapped = adaptive_wrap_line(
        &line,
        RtOptions::new(width.saturating_sub(2).max(1) as usize)
            .subsequent_indent(format!("{}  ", "  ".repeat(depth + 1)).into()),
    );
    push_owned_lines(&wrapped, out);

    for child in children {
        render_pretty_node(snapshot, child, depth + 1, width, out);
    }
}

fn append_pretty_raw_children(
    snapshot: &SpineTreeUpdatedNotification,
    parent_id: Option<&str>,
    depth: usize,
    out: &mut Vec<Line<'static>>,
) {
    for node in child_nodes(snapshot, parent_id) {
        let children = child_nodes(snapshot, Some(node.node_id.as_str()));
        let marker = pretty_marker_text(
            node,
            node.node_id == snapshot.active_node_id,
            !children.is_empty(),
        );
        out.push(Line::from(format!(
            "{}{} {}",
            "  ".repeat(depth + 1),
            marker,
            pretty_node_label_text(node)
        )));
        append_pretty_raw_children(snapshot, Some(node.node_id.as_str()), depth + 1, out);
    }
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

fn pretty_node_label(node: &SpineTreeNode) -> Span<'static> {
    if let Some(summary) = trimmed_summary(node) {
        Span::from(summary.to_string())
    } else {
        Span::from(node.node_id.clone()).cyan()
    }
}

fn pretty_node_label_text(node: &SpineTreeNode) -> String {
    trimmed_summary(node)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| node.node_id.clone())
}

fn trimmed_summary(node: &SpineTreeNode) -> Option<&str> {
    node.summary
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
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
    let status = if active {
        "current"
    } else {
        status_label(node.status)
    };
    let mut spans = vec![
        Span::from(format!("  {}{}", "  ".repeat(depth), branch(is_last))).dim(),
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
    if let Some(accounting) = format_node_accounting(node, active) {
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
        let accounting = format_node_accounting(node, active)
            .map(|accounting| format!(" {accounting}"))
            .unwrap_or_default();
        out.push(Line::from(format!(
            "{}{}{}{} {}{}",
            "  ".repeat(depth),
            marker,
            node.node_id,
            summary,
            if active {
                "current"
            } else {
                status_label(node.status)
            },
            accounting
        )));
        append_debug_raw_children(snapshot, Some(node.node_id.as_str()), depth + 1, out);
    }
}

#[cfg(debug_assertions)]
fn format_node_accounting(node: &SpineTreeNode, active: bool) -> Option<String> {
    let accounting = node.accounting.as_ref()?;
    if active && let Some(tokens) = positive_tokens(accounting.current_node_context_tokens) {
        return Some(format!("(~{} node context)", format_si_suffix(tokens)));
    }
    match (
        positive_tokens(accounting.raw_input_tokens),
        positive_tokens(accounting.memory_output_tokens),
    ) {
        (Some(raw), Some(memory)) => Some(format!(
            "(~{} raw -> ~{} memory)",
            format_si_suffix(raw),
            format_si_suffix(memory)
        )),
        (Some(raw), None) => Some(format!("(~{} raw)", format_si_suffix(raw))),
        (None, Some(memory)) => Some(format!("(~{} memory)", format_si_suffix(memory))),
        (None, None) => None,
    }
}

#[cfg(debug_assertions)]
fn positive_tokens(tokens: Option<i64>) -> Option<i64> {
    tokens.filter(|tokens| *tokens > 0)
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

#[cfg(debug_assertions)]
fn branch(is_last: bool) -> &'static str {
    if is_last { "└ " } else { "├ " }
}

#[cfg(debug_assertions)]
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
        raw_input_tokens: Option<i64>,
        memory_output_tokens: Option<i64>,
    ) -> Option<SpineTreeNodeAccounting> {
        Some(SpineTreeNodeAccounting {
            current_node_context_tokens,
            raw_input_tokens,
            memory_output_tokens,
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
        • Spine Tree  current 2.1
          ✓ earlier work
          ▾ current scope
            ◉ focused task
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
            snapshot(vec![node(
                "1",
                None,
                Some("finished"),
                SpineTreeNodeStatus::Closed,
            )]),
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
            snapshot(vec![node(
                "1",
                None,
                Some("compacted"),
                SpineTreeNodeStatus::Compacted,
            )]),
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
    fn pretty_omits_context_accounting() {
        let mut closed = node("1", None, Some("previous"), SpineTreeNodeStatus::Compacted);
        closed.accounting = accounting(None, Some(7_500), Some(1_250));
        let mut active = node("2.1", None, Some("active"), SpineTreeNodeStatus::Live);
        active.accounting = accounting(Some(181_546), None, None);
        let cell = new_spine_tree_update("turn".to_string(), snapshot(vec![closed, active]));

        let rendered = render_lines(&cell.display_lines(80)).join("\n");
        insta::assert_snapshot!(rendered, @r###"
        • Spine Tree  current 2.1
          ◌ previous
          ◉ active
        "###);
        assert!(!rendered.contains("node context"));
        assert!(!rendered.contains("raw"));
        assert!(!rendered.contains("memory"));
        assert!(!rendered.contains("compacted"));

        let raw = render_lines(&cell.raw_lines()).join("\n");
        insta::assert_snapshot!(raw, @r###"
        Spine Tree current 2.1
          ◌ previous
          ◉ active
        "###);
        assert!(!raw.contains("node context"));
        assert!(!raw.contains("raw"));
        assert!(!raw.contains("memory"));
    }

    #[cfg(debug_assertions)]
    #[test]
    fn debug_renders_context_accounting() {
        let mut closed = node("1", None, Some("previous"), SpineTreeNodeStatus::Compacted);
        closed.accounting = accounting(None, Some(7_500), Some(1_250));
        let mut active = node("2.1", None, Some("active"), SpineTreeNodeStatus::Live);
        active.accounting = accounting(Some(181_546), None, None);
        let cell = new_manual_debug_spine_tree_snapshot(snapshot(vec![closed, active]));

        let rendered = render_lines(&cell.display_lines(80)).join("\n");
        insta::assert_snapshot!(rendered, @r###"
        • Debug Spine Tree  current 2.1
          ├ 1 previous compacted (~7.50K raw -> ~1.25K memory)
          └ 2.1 active current (~182K node context)
        "###);
        assert!(rendered.contains("1 previous compacted (~7.50K raw -> ~1.25K memory)"));
        assert!(rendered.contains("2.1 active current (~182K node context)"));

        let raw = render_lines(&cell.raw_lines()).join("\n");
        insta::assert_snapshot!(raw, @r###"
        Debug Spine Tree current 2.1
        1 previous compacted (~7.50K raw -> ~1.25K memory)
        * 2.1 active current (~182K node context)
        "###);
        assert!(raw.contains("1 previous compacted (~7.50K raw -> ~1.25K memory)"));
        assert!(raw.contains("* 2.1 active current (~182K node context)"));
    }

    #[cfg(debug_assertions)]
    #[test]
    fn omits_empty_context_accounting() {
        let mut active = node("2.1", None, Some("active"), SpineTreeNodeStatus::Live);
        active.accounting = accounting(Some(0), Some(0), Some(0));
        let cell = new_manual_debug_spine_tree_snapshot(snapshot(vec![active]));

        let rendered = render_lines(&cell.display_lines(80)).join("\n");
        assert!(rendered.contains("2.1 active current"));
        assert!(!rendered.contains("node context"));
        assert!(!rendered.contains("raw"));
        assert!(!rendered.contains("memory"));
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
        assert!(rendered.contains("current 1.1.1"));
        assert!(rendered.contains("▾ 1.1"));
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
        assert!(!rendered.contains("(empty)"));
        assert!(rendered.contains("current 1"));
        assert!(rendered.contains("◉ 1"));
        assert!(rendered.contains("✓ 1.1"));
        assert!(!rendered.contains("root"));
    }
}
