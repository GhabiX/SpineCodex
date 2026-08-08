//! Detailed host-only rendering for the hidden `/debugspine` command.

use super::*;
use codex_app_server_protocol::SpineTreeNodeKind;
use codex_protocol::num_format::format_si_suffix;

pub(super) fn display_lines(
    snapshot: &SpineTreeUpdatedNotification,
    width: u16,
    node_id: Option<&str>,
) -> Vec<Line<'static>> {
    let mut lines = vec![match node_id {
        Some(node_id) => node_header(snapshot, node_id),
        None => header(snapshot),
    }];
    if let Err(error) = validate_spine_tree_snapshot(snapshot) {
        lines.push(invalid_snapshot_display_line(error));
        return lines;
    }

    if let Some(node_id) = node_id {
        let Some(node) = snapshot.nodes.iter().find(|node| node.node_id == node_id) else {
            lines.push(node_not_found_line(node_id));
            return lines;
        };
        append_node_details(snapshot, node, width, &mut lines);
        return lines;
    }

    let root_nodes = child_nodes(snapshot, None);
    if root_nodes.is_empty() {
        lines.push(vec!["  └ ".dim(), "(empty)".dim().italic()].into());
        return lines;
    }

    let root_count = root_nodes.len();
    for (index, node) in root_nodes.into_iter().enumerate() {
        render_node(
            snapshot,
            node,
            "  ",
            index + 1 == root_count,
            width,
            &mut lines,
        );
    }
    lines
}

pub(super) fn raw_lines(
    snapshot: &SpineTreeUpdatedNotification,
    node_id: Option<&str>,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(match node_id {
        Some(node_id) => format!(
            "Debug Spine Node {node_id} current {}",
            snapshot.active_node_id
        ),
        None => format!("Debug Spine Tree current {}", snapshot.active_node_id),
    })];
    if let Err(error) = validate_spine_tree_snapshot(snapshot) {
        lines.push(Line::from(invalid_snapshot_message(error)));
        return lines;
    }

    if let Some(node_id) = node_id {
        let Some(node) = snapshot.nodes.iter().find(|node| node.node_id == node_id) else {
            lines.push(Line::from(node_not_found_message(node_id)));
            return lines;
        };
        append_raw_node_details(snapshot, node, &mut lines);
        return lines;
    }

    append_raw_children(snapshot, None, 0, &mut lines);
    lines
}

fn header(snapshot: &SpineTreeUpdatedNotification) -> Line<'static> {
    vec![
        "• ".dim(),
        "Debug Spine Tree".bold(),
        "  ".dim(),
        "current ".dim(),
        Span::from(snapshot.active_node_id.clone()).cyan().bold(),
    ]
    .into()
}

fn node_header(snapshot: &SpineTreeUpdatedNotification, node_id: &str) -> Line<'static> {
    vec![
        "• ".dim(),
        "Debug Spine Node".bold(),
        "  ".dim(),
        Span::from(node_id.to_string()).cyan().bold(),
        "  current ".dim(),
        Span::from(snapshot.active_node_id.clone()).cyan().bold(),
    ]
    .into()
}

fn render_node(
    snapshot: &SpineTreeUpdatedNotification,
    node: &SpineTreeNode,
    prefix: &str,
    is_last: bool,
    width: u16,
    out: &mut Vec<Line<'static>>,
) {
    let active = node.node_id == snapshot.active_node_id;
    let status = status_label(node.status, active);
    let line_prefix = format!("{}{}", prefix, pretty_branch(is_last));
    let child_prefix = format!("{}{}", prefix, pretty_child_prefix(is_last));
    let mut spans = vec![
        Span::from(line_prefix).dim(),
        Span::from(node.node_id.clone()).cyan().bold(),
    ];
    if let Some(summary) = trimmed_summary(node) {
        spans.push(" ".into());
        spans.push(Span::from(summary.to_string()));
    }
    spans.push(" ".into());
    let status_span = if active {
        Span::from(status).cyan().bold()
    } else {
        Span::from(status).dim()
    };
    spans.push(status_span);
    spans.push(" ".into());
    spans.push(Span::from(format_node_details(node)).dim());

    let line = Line::from(spans);
    let wrapped = adaptive_wrap_line(
        &line,
        RtOptions::new(width.saturating_sub(2).max(1) as usize)
            .subsequent_indent(format!("{child_prefix}  ").into()),
    );
    push_owned_lines(&wrapped, out);

    let children = child_nodes(snapshot, Some(node.node_id.as_str()));
    let child_count = children.len();
    for (index, child) in children.into_iter().enumerate() {
        render_node(
            snapshot,
            child,
            &child_prefix,
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
        let summary = trimmed_summary(node)
            .map(|summary| format!(" {summary}"))
            .unwrap_or_default();
        let active = node.node_id == snapshot.active_node_id;
        out.push(Line::from(format!(
            "{}{}{}{} {} {}",
            "  ".repeat(depth),
            marker,
            node.node_id,
            summary,
            status_label(node.status, active),
            format_node_details(node),
        )));
        append_raw_children(snapshot, Some(node.node_id.as_str()), depth + 1, out);
    }
}

fn format_node_details(node: &SpineTreeNode) -> String {
    let kind = kind_label(node.kind);
    let range = match node.end {
        Some(end) => format!("rollout {}..{end}", node.start),
        None => format!("rollout {}..", node.start),
    };
    let mut details = vec![kind.to_string(), range];
    if let Some(pressure) = node.context_pressure.as_ref() {
        if let Some(tokens) = pressure.context_tokens.filter(|tokens| *tokens >= 0) {
            details.push(format!("~{} inclusive context", format_si_suffix(tokens)));
        } else if let Some(problem) = pressure.problem {
            details.push(format!(
                "context problem: {}",
                context_problem_label(problem)
            ));
        }
    }
    format!("({})", details.join(", "))
}

fn append_node_details(
    snapshot: &SpineTreeUpdatedNotification,
    node: &SpineTreeNode,
    width: u16,
    out: &mut Vec<Line<'static>>,
) {
    for (label, value) in node_detail_fields(snapshot, node) {
        let line = Line::from(vec![
            "  ".into(),
            Span::from(format!("{label}: ")).dim(),
            Span::from(value),
        ]);
        let wrapped = adaptive_wrap_line(
            &line,
            RtOptions::new(width.saturating_sub(2).max(1) as usize)
                .subsequent_indent("    ".into()),
        );
        push_owned_lines(&wrapped, out);
    }

    let Some(memory) = node
        .memory_summary
        .as_deref()
        .filter(|memory| !memory.trim().is_empty())
    else {
        return;
    };
    out.push(Line::from(vec!["  ".into(), "memory:".dim()]));
    for memory_line in memory.lines() {
        let line = Line::from(format!("    {memory_line}"));
        let wrapped = adaptive_wrap_line(
            &line,
            RtOptions::new(width.saturating_sub(2).max(1) as usize)
                .subsequent_indent("    ".into()),
        );
        push_owned_lines(&wrapped, out);
    }
}

fn append_raw_node_details(
    snapshot: &SpineTreeUpdatedNotification,
    node: &SpineTreeNode,
    out: &mut Vec<Line<'static>>,
) {
    out.extend(
        node_detail_fields(snapshot, node)
            .into_iter()
            .map(|(label, value)| Line::from(format!("  {label}: {value}"))),
    );
    if let Some(memory) = node
        .memory_summary
        .as_deref()
        .filter(|memory| !memory.trim().is_empty())
    {
        out.push(Line::from("  memory:"));
        out.extend(memory.lines().map(|line| Line::from(format!("    {line}"))));
    }
}

fn node_detail_fields(
    snapshot: &SpineTreeUpdatedNotification,
    node: &SpineTreeNode,
) -> Vec<(&'static str, String)> {
    let mut fields = vec![
        ("id", node.node_id.clone()),
        (
            "parent",
            node.parent_id
                .clone()
                .unwrap_or_else(|| "(root)".to_string()),
        ),
        ("kind", kind_label(node.kind).to_string()),
        (
            "status",
            if node.node_id == snapshot.active_node_id {
                format!("{} (current)", node_status_label(node.status))
            } else {
                node_status_label(node.status).to_string()
            },
        ),
    ];
    if let Some(summary) = trimmed_summary(node) {
        fields.push(("summary", summary.to_string()));
    }
    fields.push((
        "rollout",
        match node.end {
            Some(end) => format!("{}..{end}", node.start),
            None => format!("{}..", node.start),
        },
    ));
    if let Some(pressure) = node.context_pressure.as_ref() {
        if let Some(tokens) = pressure.open_input_tokens {
            fields.push(("open input tokens", tokens.to_string()));
        }
        if let Some(tokens) = pressure.current_input_tokens {
            fields.push(("current input tokens", tokens.to_string()));
        }
        if let Some(tokens) = pressure.context_tokens {
            fields.push(("inclusive context tokens", tokens.to_string()));
        }
        if let Some(problem) = pressure.problem {
            fields.push((
                "context problem",
                context_problem_label(problem).to_string(),
            ));
        }
    }
    fields
}

fn node_not_found_line(node_id: &str) -> Line<'static> {
    vec![
        "  ".into(),
        Span::from(node_not_found_message(node_id)).red(),
    ]
    .into()
}

fn node_not_found_message(node_id: &str) -> String {
    format!("Spine node `{node_id}` was not found in the current tree.")
}

fn kind_label(kind: SpineTreeNodeKind) -> &'static str {
    match kind {
        SpineTreeNodeKind::RootEpoch => "root epoch",
        SpineTreeNodeKind::Task => "task",
    }
}

fn node_status_label(status: SpineTreeNodeStatus) -> &'static str {
    match status {
        SpineTreeNodeStatus::Live => "live",
        SpineTreeNodeStatus::Opened => "opened",
        SpineTreeNodeStatus::Closed => "closed",
        SpineTreeNodeStatus::Compacted => "compacted",
    }
}

fn context_problem_label(
    problem: codex_app_server_protocol::SpineNodeContextPressureProblem,
) -> &'static str {
    match problem {
        codex_app_server_protocol::SpineNodeContextPressureProblem::MissingCurrentUsage => {
            "missing current usage"
        }
        codex_app_server_protocol::SpineNodeContextPressureProblem::MissingOpenContextBaseline => {
            "missing open baseline"
        }
        codex_app_server_protocol::SpineNodeContextPressureProblem::CoordinateMismatch => {
            "coordinate mismatch"
        }
    }
}

fn status_label(status: SpineTreeNodeStatus, active: bool) -> &'static str {
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
#[path = "spine_tree_debug_tests.rs"]
mod tests;
