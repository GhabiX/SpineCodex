use crate::render::line_utils::push_owned_lines;
use crate::wrapping::RtOptions;
use crate::wrapping::adaptive_wrap_line;
use codex_app_server_protocol::SpineTreeNode;
use codex_app_server_protocol::SpineTreeNodeStatus;
use codex_app_server_protocol::SpineTreePlanCheckpoint;
use codex_app_server_protocol::SpineTreePlanItem;
use codex_app_server_protocol::SpineTreePlanItemStatus;
use codex_app_server_protocol::SpineTreePlanTree;
use codex_app_server_protocol::SpineTreePlanTreeScope;
use codex_app_server_protocol::SpineTreeUpdatedNotification;
use ratatui::prelude::*;
use ratatui::style::Style;
use ratatui::style::Styled;
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

    let plantree = spine_tree_active_plantree(snapshot);
    let root_count = root_nodes.len();
    for (index, node) in root_nodes.into_iter().enumerate() {
        render_spine_tree_node(
            snapshot,
            node,
            0,
            index + 1 == root_count,
            &snapshot.active_node_id,
            width,
            plantree,
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
        if let Some(plan) = &node.node.plan {
            for item in &plan.items {
                lines.push(Line::from(format!(
                    "{prefix}  {:?}: {}",
                    item.status, item.step
                )));
            }
            if let Some(spine_plantree) = &plan.spine_plantree {
                lines.push(Line::from(format!(
                    "{prefix}  plantree anchor={}",
                    spine_plantree.anchor_node_id
                )));
                append_spine_tree_plantree_raw_lines(
                    &mut lines,
                    &spine_plantree.root,
                    node.depth + 1,
                );
            }
        }
    }
    lines
}

pub(super) fn plantree_display_lines(
    snapshot: &SpineTreeUpdatedNotification,
    width: u16,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = vec![vec!["• ".dim(), "Spine PlanTree".bold()].into()];
    let Some(plantree) = spine_tree_active_plantree(snapshot) else {
        lines.push(vec!["  └ ".dim(), "(empty)".dim().italic()].into());
        return lines;
    };

    let root_id = plantree
        .root
        .existing_node_id
        .as_deref()
        .unwrap_or(plantree.anchor_node_id.as_str())
        .to_string();
    render_spine_plantree_scope(
        snapshot,
        &plantree.root,
        root_id,
        0,
        true,
        width,
        &mut lines,
    );
    lines
}

pub(super) fn plantree_raw_lines(snapshot: &SpineTreeUpdatedNotification) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(format!(
        "Spine PlanTree turn={} snapshot_seq={} active_node={}",
        snapshot.turn_id, snapshot.snapshot_seq, snapshot.active_node_id
    ))];
    if let Some(plantree) = spine_tree_active_plantree(snapshot) {
        lines.push(Line::from(format!(
            "plantree anchor={}",
            plantree.anchor_node_id
        )));
        append_spine_tree_plantree_raw_lines(&mut lines, &plantree.root, 0);
    } else {
        lines.push(Line::from("(empty)"));
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

fn spine_tree_active_plantree(
    snapshot: &SpineTreeUpdatedNotification,
) -> Option<&SpineTreePlanTree> {
    snapshot
        .nodes
        .iter()
        .find(|node| node.node_id == snapshot.active_node_id)
        .and_then(|node| node.plan.as_ref())
        .and_then(|plan| plan.spine_plantree.as_ref())
}

fn render_spine_tree_node(
    snapshot: &SpineTreeUpdatedNotification,
    node: &SpineTreeNode,
    depth: usize,
    is_last: bool,
    active_node_id: &str,
    width: u16,
    plantree: Option<&SpineTreePlanTree>,
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

    if let Some(plan) = &node.plan {
        for item in &plan.items {
            out.extend(render_spine_tree_plan_item(item, depth + 1, width));
        }
    }

    let real_children = spine_tree_child_nodes(snapshot, Some(node.node_id.as_str()));
    let planned_children = planned_future_child_scopes(plantree, node.node_id.as_str());
    let real_child_count = real_children.len();
    let planned_child_count = planned_children.len();
    for (index, child) in real_children.into_iter().enumerate() {
        render_spine_tree_node(
            snapshot,
            child,
            depth + 1,
            index + 1 == real_child_count && planned_child_count == 0,
            active_node_id,
            width,
            plantree,
            out,
        );
    }

    let mut planned_index = next_planned_child_index(snapshot, node.node_id.as_str());
    for (index, scope) in planned_children.into_iter().enumerate() {
        let predicted_id = format!("~{}.{}", node.node_id, planned_index);
        planned_index = planned_index.saturating_add(1);
        render_spine_tree_planned_scope(
            scope,
            predicted_id,
            depth + 1,
            index + 1 == planned_child_count,
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
        SpineTreeNodeStatus::Live | SpineTreeNodeStatus::Opened => "live",
        SpineTreeNodeStatus::Finished => "finished",
        SpineTreeNodeStatus::Closed => "closed",
    }
}

fn planned_future_child_scopes<'a>(
    plantree: Option<&'a SpineTreePlanTree>,
    node_id: &str,
) -> Vec<&'a SpineTreePlanTreeScope> {
    plantree
        .and_then(|plantree| find_plantree_scope_for_node(plantree, node_id))
        .map(|scope| {
            scope
                .children
                .iter()
                .filter(|child| child.existing_node_id.is_none())
                .collect()
        })
        .unwrap_or_default()
}

fn find_plantree_scope_for_node<'a>(
    plantree: &'a SpineTreePlanTree,
    node_id: &str,
) -> Option<&'a SpineTreePlanTreeScope> {
    if plantree
        .root
        .existing_node_id
        .as_deref()
        .unwrap_or(plantree.anchor_node_id.as_str())
        == node_id
    {
        return Some(&plantree.root);
    }
    find_plantree_child_scope_for_node(&plantree.root, node_id)
}

fn find_plantree_child_scope_for_node<'a>(
    scope: &'a SpineTreePlanTreeScope,
    node_id: &str,
) -> Option<&'a SpineTreePlanTreeScope> {
    for child in &scope.children {
        if child.existing_node_id.as_deref() == Some(node_id) {
            return Some(child);
        }
        if let Some(found) = find_plantree_child_scope_for_node(child, node_id) {
            return Some(found);
        }
    }
    None
}

fn next_planned_child_index(snapshot: &SpineTreeUpdatedNotification, parent_id: &str) -> u32 {
    snapshot
        .nodes
        .iter()
        .filter(|node| node.parent_id.as_deref() == Some(parent_id))
        .filter_map(|node| node.node_id.rsplit('.').next())
        .filter_map(|segment| segment.parse::<u32>().ok())
        .max()
        .unwrap_or(0)
        .saturating_add(1)
}

fn append_spine_tree_plantree_raw_lines(
    lines: &mut Vec<Line<'static>>,
    scope: &SpineTreePlanTreeScope,
    depth: usize,
) {
    let prefix = "  ".repeat(depth);
    let scope_id = scope.existing_node_id.as_deref().unwrap_or("future");
    lines.push(Line::from(format!(
        "{prefix}plantree [{scope_id}]: {}",
        scope.summary
    )));
    for checkpoint in &scope.checkpoints {
        lines.push(Line::from(format!(
            "{prefix}  {:?}: {}",
            checkpoint.status, checkpoint.task
        )));
    }
    for child in &scope.children {
        append_spine_tree_plantree_raw_lines(lines, child, depth + 1);
    }
}

fn render_spine_tree_planned_scope(
    scope: &SpineTreePlanTreeScope,
    predicted_id: String,
    depth: usize,
    is_last: bool,
    width: u16,
    out: &mut Vec<Line<'static>>,
) {
    let branch = spine_tree_branch(depth, is_last);
    let prefix = format!("  {}{}", "  ".repeat(depth), branch);
    let opts = RtOptions::new(width.saturating_sub(2).max(1) as usize)
        .subsequent_indent(format!("{}  ", "  ".repeat(depth + 1)).into());
    let line = Line::from(vec![
        Span::from(prefix).dim(),
        Span::from(predicted_id.clone()).yellow().bold(),
        Span::from(" "),
        Span::from(scope.summary.clone()),
    ]);
    let wrapped = adaptive_wrap_line(&line, opts);
    push_owned_lines(&wrapped, out);

    for checkpoint in &scope.checkpoints {
        out.extend(render_spine_tree_planned_checkpoint(
            checkpoint,
            depth + 1,
            width,
        ));
    }
    let future_children = scope
        .children
        .iter()
        .filter(|child| child.existing_node_id.is_none())
        .collect::<Vec<_>>();
    let child_count = future_children.len();
    for (index, child) in future_children.into_iter().enumerate() {
        render_spine_tree_planned_scope(
            child,
            format!("{}.{}", predicted_id, index + 1),
            depth + 1,
            index + 1 == child_count,
            width,
            out,
        );
    }
}

fn render_spine_plantree_scope(
    snapshot: &SpineTreeUpdatedNotification,
    scope: &SpineTreePlanTreeScope,
    display_id: String,
    depth: usize,
    is_last: bool,
    width: u16,
    out: &mut Vec<Line<'static>>,
) {
    let branch = spine_tree_branch(depth, is_last);
    let prefix = format!("  {}{}", "  ".repeat(depth), branch);
    let opts = RtOptions::new(width.saturating_sub(2).max(1) as usize)
        .subsequent_indent(format!("{}  ", "  ".repeat(depth + 1)).into());
    let id_style = if display_id.starts_with('~') {
        Style::default().yellow().bold()
    } else {
        Style::default().cyan().bold()
    };
    let line = Line::from(vec![
        Span::from(prefix).dim(),
        Span::from(display_id.clone()).style(id_style),
        Span::from(" "),
        Span::from(scope.summary.clone()),
    ]);
    let wrapped = adaptive_wrap_line(&line, opts);
    push_owned_lines(&wrapped, out);

    for checkpoint in &scope.checkpoints {
        out.extend(render_spine_tree_planned_checkpoint(
            checkpoint,
            depth + 1,
            width,
        ));
    }

    let child_count = scope.children.len();
    let mut next_future_index = scope
        .existing_node_id
        .as_deref()
        .map(|node_id| next_planned_child_index(snapshot, node_id))
        .unwrap_or(1);
    for (index, child) in scope.children.iter().enumerate() {
        let child_id = if let Some(existing_node_id) = &child.existing_node_id {
            existing_node_id.clone()
        } else {
            let predicted_id = predicted_plantree_child_id(&display_id, next_future_index);
            next_future_index = next_future_index.saturating_add(1);
            predicted_id
        };
        render_spine_plantree_scope(
            snapshot,
            child,
            child_id,
            depth + 1,
            index + 1 == child_count,
            width,
            out,
        );
    }
}

fn predicted_plantree_child_id(parent_display_id: &str, child_index: u32) -> String {
    if parent_display_id.starts_with('~') {
        format!("{parent_display_id}.{child_index}")
    } else {
        format!("~{parent_display_id}.{child_index}")
    }
}

fn render_spine_tree_planned_checkpoint(
    checkpoint: &SpineTreePlanCheckpoint,
    depth: usize,
    width: u16,
) -> Vec<Line<'static>> {
    let (box_str, step_style) = match checkpoint.status {
        SpineTreePlanItemStatus::Completed => ("· ", Style::default().crossed_out().dim()),
        SpineTreePlanItemStatus::InProgress => ("· ", Style::default()),
        SpineTreePlanItemStatus::Pending => ("· ", Style::default().dim()),
    };
    let indent = format!("  {}  ", "  ".repeat(depth));
    let opts = RtOptions::new(width.saturating_sub(2).max(1) as usize)
        .initial_indent(format!("{indent}{box_str}").into())
        .subsequent_indent(format!("{indent}  ").into());
    let line = Line::from(checkpoint.task.clone().set_style(step_style));
    let wrapped = adaptive_wrap_line(&line, opts);
    let mut out = Vec::new();
    push_owned_lines(&wrapped, &mut out);
    out
}

fn render_spine_tree_plan_item(
    item: &SpineTreePlanItem,
    depth: usize,
    width: u16,
) -> Vec<Line<'static>> {
    let (box_str, step_style) = match item.status {
        SpineTreePlanItemStatus::Completed => ("✔ ", Style::default().crossed_out().dim()),
        SpineTreePlanItemStatus::InProgress => ("□ ", Style::default().cyan().bold()),
        SpineTreePlanItemStatus::Pending => ("□ ", Style::default().dim()),
    };
    let indent = format!("  {}  ", "  ".repeat(depth));
    let opts = RtOptions::new(width.saturating_sub(2).max(1) as usize)
        .initial_indent(format!("{indent}{box_str}").into())
        .subsequent_indent(format!("{indent}  ").into());
    let line = Line::from(item.step.clone().set_style(step_style));
    let wrapped = adaptive_wrap_line(&line, opts);
    let mut out = Vec::new();
    push_owned_lines(&wrapped, &mut out);
    out
}
