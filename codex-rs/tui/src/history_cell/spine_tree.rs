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

#[cfg(test)]
mod tests {
    use super::*;
    use codex_app_server_protocol::SpineTreePlan;
    use ratatui::style::Color;
    use ratatui::style::Modifier;

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

    fn span_style_containing(lines: &[Line<'static>], text: &str) -> Style {
        lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .find(|span| span.content.contains(text))
            .map(|span| span.style)
            .unwrap_or_else(|| panic!("missing span containing {text:?}"))
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
        plan: Option<SpineTreePlan>,
    ) -> SpineTreeNode {
        SpineTreeNode {
            node_id: node_id.to_string(),
            parent_id: parent_id.map(str::to_string),
            summary: summary.map(str::to_string),
            status,
            plan,
        }
    }

    fn plan_item(step: &str, status: SpineTreePlanItemStatus) -> SpineTreePlanItem {
        SpineTreePlanItem {
            stable_task_id: step.to_string(),
            step: step.to_string(),
            status,
        }
    }

    fn plan(
        items: Vec<SpineTreePlanItem>,
        spine_plantree: Option<SpineTreePlanTree>,
    ) -> SpineTreePlan {
        SpineTreePlan {
            revision: 1,
            explanation: None,
            spine_plantree,
            items,
        }
    }

    fn plantree(anchor_node_id: &str, root: SpineTreePlanTreeScope) -> SpineTreePlanTree {
        SpineTreePlanTree {
            anchor_node_id: anchor_node_id.to_string(),
            root,
        }
    }

    fn scope(
        existing_node_id: Option<&str>,
        summary: &str,
        children: Vec<SpineTreePlanTreeScope>,
    ) -> SpineTreePlanTreeScope {
        SpineTreePlanTreeScope {
            existing_node_id: existing_node_id.map(str::to_string),
            summary: summary.to_string(),
            status: None,
            checkpoints: Vec::new(),
            children,
        }
    }

    fn scope_with_checkpoint(
        existing_node_id: Option<&str>,
        summary: &str,
        checkpoint_task: &str,
        checkpoint_status: SpineTreePlanItemStatus,
    ) -> SpineTreePlanTreeScope {
        SpineTreePlanTreeScope {
            existing_node_id: existing_node_id.map(str::to_string),
            summary: summary.to_string(),
            status: None,
            checkpoints: vec![SpineTreePlanCheckpoint {
                task: checkpoint_task.to_string(),
                status: checkpoint_status,
            }],
            children: Vec::new(),
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
                    SpineTreeNodeStatus::Opened,
                    None,
                ),
                node(
                    "1.1",
                    Some("1"),
                    Some("focused scope with narrow wrapping visible token"),
                    SpineTreeNodeStatus::Live,
                    Some(plan(
                        vec![plan_item(
                            "validate narrow wrapping token",
                            SpineTreePlanItemStatus::InProgress,
                        )],
                        None,
                    )),
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
        assert!(joined.contains("validate"));
    }

    #[test]
    fn tree_display_handles_deep_tree_and_missing_summary() {
        let snapshot = snapshot(
            "1.1.1.1",
            vec![
                node("1", None, None, SpineTreeNodeStatus::Opened, None),
                node(
                    "1.1",
                    Some("1"),
                    Some(" "),
                    SpineTreeNodeStatus::Opened,
                    None,
                ),
                node(
                    "1.1.1",
                    Some("1.1"),
                    Some("nested scope"),
                    SpineTreeNodeStatus::Finished,
                    None,
                ),
                node(
                    "1.1.1.1",
                    Some("1.1.1"),
                    None,
                    SpineTreeNodeStatus::Live,
                    None,
                ),
            ],
        );

        let rendered = render_lines(&tree_display_lines(&snapshot, /*width*/ 80)).join("\n");

        assert!(rendered.contains("1 live"));
        assert!(rendered.contains("1.1 live"));
        assert!(rendered.contains("1.1.1 nested scope finished"));
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
                    None,
                ),
                node(
                    "2",
                    None,
                    Some("current epoch"),
                    SpineTreeNodeStatus::Opened,
                    None,
                ),
                node(
                    "2.1",
                    Some("2"),
                    Some("focused leaf"),
                    SpineTreeNodeStatus::Live,
                    None,
                ),
            ],
        );

        let rendered = render_lines(&tree_display_lines(&snapshot, /*width*/ 80));

        assert!(
            rendered[1].starts_with("  ├ 1 archived epoch closed"),
            "expected first root epoch to use a non-last branch and show status, got: {rendered:?}"
        );
        assert!(
            rendered[2].starts_with("  └ 2 current epoch live"),
            "expected second root epoch to use a last branch and show status, got: {rendered:?}"
        );
        assert!(rendered[3].contains("2.1 focused leaf current"));
    }

    #[test]
    fn tree_display_uses_active_node_plantree_only() {
        let stale_plan = plan(
            Vec::new(),
            Some(plantree(
                "1",
                scope(
                    Some("1"),
                    "root scope",
                    vec![scope(None, "stale future scope", Vec::new())],
                ),
            )),
        );
        let active_plan = plan(
            Vec::new(),
            Some(plantree(
                "1.1",
                scope(
                    Some("1.1"),
                    "active scope",
                    vec![scope(None, "active future scope", Vec::new())],
                ),
            )),
        );
        let snapshot = snapshot(
            "1.1",
            vec![
                node(
                    "1",
                    None,
                    Some("root"),
                    SpineTreeNodeStatus::Opened,
                    Some(stale_plan),
                ),
                node(
                    "1.1",
                    Some("1"),
                    Some("focused leaf"),
                    SpineTreeNodeStatus::Live,
                    Some(active_plan),
                ),
            ],
        );

        let rendered = render_lines(&tree_display_lines(&snapshot, /*width*/ 80)).join("\n");

        assert!(rendered.contains("~1.1.1 active future scope"));
        assert!(!rendered.contains("stale future scope"));
    }

    #[test]
    fn tree_display_predicts_future_ids_after_real_children() {
        let active_plan = plan(
            Vec::new(),
            Some(plantree(
                "1.1",
                scope(
                    Some("1.1"),
                    "active scope",
                    vec![scope(
                        None,
                        "planned sibling",
                        vec![scope(None, "planned nested", Vec::new())],
                    )],
                ),
            )),
        );
        let snapshot = snapshot(
            "1.1",
            vec![
                node("1", None, Some("root"), SpineTreeNodeStatus::Opened, None),
                node(
                    "1.1",
                    Some("1"),
                    Some("focused leaf"),
                    SpineTreeNodeStatus::Live,
                    Some(active_plan),
                ),
                node(
                    "1.1.1",
                    Some("1.1"),
                    Some("real child"),
                    SpineTreeNodeStatus::Opened,
                    None,
                ),
            ],
        );

        let rendered = render_lines(&tree_display_lines(&snapshot, /*width*/ 80)).join("\n");

        assert!(rendered.contains("1.1.1 real child live"));
        assert!(rendered.contains("~1.1.2 planned sibling"));
        assert!(rendered.contains("~1.1.2.1 planned nested"));
        assert!(!rendered.contains("~1.1.1 planned sibling"));
    }

    #[test]
    fn plantree_display_predicts_future_ids_after_real_children() {
        let active_plan = plan(
            Vec::new(),
            Some(plantree(
                "1.1",
                scope(
                    Some("1.1"),
                    "active scope",
                    vec![scope(
                        None,
                        "planned sibling",
                        vec![scope(None, "planned nested", Vec::new())],
                    )],
                ),
            )),
        );
        let snapshot = snapshot(
            "1.1",
            vec![
                node("1", None, Some("root"), SpineTreeNodeStatus::Opened, None),
                node(
                    "1.1",
                    Some("1"),
                    Some("focused leaf"),
                    SpineTreeNodeStatus::Live,
                    Some(active_plan),
                ),
                node(
                    "1.1.1",
                    Some("1.1"),
                    Some("real child"),
                    SpineTreeNodeStatus::Opened,
                    None,
                ),
            ],
        );

        let rendered = render_lines(&plantree_display_lines(&snapshot, /*width*/ 80)).join("\n");

        assert!(rendered.contains("Spine PlanTree"));
        assert!(rendered.contains("1.1 active scope"));
        assert!(rendered.contains("~1.1.2 planned sibling"));
        assert!(rendered.contains("~1.1.2.1 planned nested"));
        assert!(!rendered.contains("Spine Tree"));
    }

    #[test]
    fn planned_checkpoint_in_progress_does_not_use_active_style() {
        let active_plan = plan(
            vec![plan_item(
                "real active checklist item",
                SpineTreePlanItemStatus::InProgress,
            )],
            Some(plantree(
                "1.1",
                scope(
                    Some("1.1"),
                    "focused work",
                    vec![scope_with_checkpoint(
                        None,
                        "planned future scope",
                        "planned in-progress metadata",
                        SpineTreePlanItemStatus::InProgress,
                    )],
                ),
            )),
        );
        let snapshot = snapshot(
            "1.1",
            vec![
                node(
                    "1",
                    None,
                    Some("root scope"),
                    SpineTreeNodeStatus::Opened,
                    None,
                ),
                node(
                    "1.1",
                    Some("1"),
                    Some("focused work"),
                    SpineTreeNodeStatus::Live,
                    Some(active_plan),
                ),
            ],
        );

        let lines = tree_display_lines(&snapshot, /*width*/ 80);
        let rendered = render_lines(&lines).join("\n");
        assert!(rendered.contains("~1.1.1 planned future scope"));
        assert!(!rendered.contains("footprint:"));

        let real_style = span_style_containing(&lines, "real active checklist item");
        assert_eq!(real_style.fg, Some(Color::Cyan));
        assert!(real_style.add_modifier.contains(Modifier::BOLD));

        let planned_style = span_style_containing(&lines, "planned in-progress metadata");
        assert_eq!(planned_style.fg, None);
        assert!(!planned_style.add_modifier.contains(Modifier::BOLD));
    }
}
