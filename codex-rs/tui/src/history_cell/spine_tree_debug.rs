//! Detailed host-only rendering for the hidden `/debugspine` command.

use super::*;
use codex_app_server_protocol::SpineTreeNodeKind;
use codex_protocol::num_format::format_si_suffix;

pub(super) fn display_lines(
    snapshot: &SpineTreeUpdatedNotification,
    width: u16,
) -> Vec<Line<'static>> {
    let mut lines = vec![header(snapshot)];
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

pub(super) fn raw_lines(snapshot: &SpineTreeUpdatedNotification) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(format!(
        "Debug Spine Tree current {}",
        snapshot.active_node_id
    ))];
    if let Err(error) = validate_spine_tree_snapshot(snapshot) {
        lines.push(Line::from(invalid_snapshot_message(error)));
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
    let kind = match node.kind {
        SpineTreeNodeKind::RootEpoch => "root epoch",
        SpineTreeNodeKind::Task => "task",
    };
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
    if let Some(memory_summary) = node
        .memory_summary
        .as_deref()
        .map(str::trim)
        .filter(|summary| !summary.is_empty())
    {
        details.push(format!("memory: {memory_summary}"));
    }
    format!("({})", details.join(", "))
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
