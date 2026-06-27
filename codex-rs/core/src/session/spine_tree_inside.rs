use crate::spine::NodeId;
use crate::spine::bridge::OpenNodeContextProjection;
use crate::spine::bridge::TreeSnapshotProjection;
use codex_protocol::num_format::format_si_suffix;
use codex_protocol::protocol::TokenUsageInfo;
use codex_protocol::spine_tree::SpineNodeContextProblem;
use codex_protocol::spine_tree::SpineTreeNodeAccountingSnapshot;
use codex_protocol::spine_tree::SpineTreeUpdateEvent;
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub(crate) struct SpineTreeInsideView {
    pub(crate) rendered_tree: String,
}

#[derive(Clone, Debug)]
pub(crate) struct SpineTreePressureView {
    pub(crate) active_node_id: String,
    pub(crate) active_node_summary: Option<String>,
    pub(crate) open_nodes: Vec<SpineOpenNodeInside>,
    pub(crate) context_window: Option<SpineContextWindowInside>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpineOpenNodeInside {
    pub(crate) node_id: NodeId,
    pub(crate) summary: Option<String>,
    pub(crate) provider_input_tokens: Option<i64>,
    pub(crate) current_node_context_tokens: Option<i64>,
    pub(crate) problem: Option<SpineNodeContextProblem>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpineContextWindowInside {
    pub(crate) context_tokens: i64,
    pub(crate) model_context_window: Option<i64>,
    remaining_percent: Option<i64>,
}

pub(crate) fn build_spine_tree_inside_view_from_projection(
    projection: TreeSnapshotProjection,
    mut rendered_tree: String,
    token_info: Option<&TokenUsageInfo>,
) -> SpineTreeInsideView {
    let (_snapshot, _open_node_projections) = projection.into_parts();

    let context_window = context_window_inside(token_info);
    if let Some(line) = format_context_window_pressure(context_window.as_ref()) {
        rendered_tree.push_str("\n\n");
        rendered_tree.push_str(&line);
    }

    SpineTreeInsideView { rendered_tree }
}

pub(crate) fn build_spine_tree_context_annotations(
    projection: &TreeSnapshotProjection,
    token_info: Option<&TokenUsageInfo>,
) -> BTreeMap<NodeId, String> {
    let open_nodes =
        build_open_nodes_inside(projection.snapshot(), token_info, projection.open_nodes());
    format_open_node_context_annotations(&open_nodes)
}

pub(crate) fn build_spine_tree_pressure_view_from_projection(
    projection: TreeSnapshotProjection,
    token_info: Option<&TokenUsageInfo>,
) -> SpineTreePressureView {
    let active_node_id = projection.snapshot().active_node_id.clone();
    let active_node_summary = snapshot_node_summary(
        projection.snapshot(),
        projection.snapshot().active_node_id.as_str(),
    );
    let open_nodes =
        build_open_nodes_inside(projection.snapshot(), token_info, projection.open_nodes());
    SpineTreePressureView {
        active_node_id,
        active_node_summary,
        open_nodes,
        context_window: context_window_inside(token_info),
    }
}

pub(crate) fn annotate_spine_tree_snapshot(
    projection: TreeSnapshotProjection,
    token_info: Option<&TokenUsageInfo>,
) -> SpineTreeUpdateEvent {
    let (mut snapshot, open_node_projections) = projection.into_parts();
    annotate_open_node_contexts(&mut snapshot, token_info, &open_node_projections);
    snapshot
}

pub(crate) fn node_context_tokens(
    current: Option<&TokenUsageInfo>,
    open_context_tokens: Option<i64>,
) -> Result<i64, SpineNodeContextProblem> {
    let current =
        provider_input_context_tokens(current.ok_or(SpineNodeContextProblem::MissingCurrentUsage)?)
            .ok_or(SpineNodeContextProblem::MissingCurrentUsage)?;
    let open_context_tokens =
        open_context_tokens.ok_or(SpineNodeContextProblem::MissingOpenContextBaseline)?;
    if current < open_context_tokens {
        return Err(SpineNodeContextProblem::CoordinateMismatch);
    }
    Ok(current - open_context_tokens)
}

fn provider_input_context_tokens(current: &TokenUsageInfo) -> Option<i64> {
    let input_tokens = current.last_token_usage.input_tokens;
    (input_tokens > 0).then_some(input_tokens)
}

fn build_open_nodes_inside(
    snapshot: &SpineTreeUpdateEvent,
    current: Option<&TokenUsageInfo>,
    open_nodes: &[OpenNodeContextProjection],
) -> Vec<SpineOpenNodeInside> {
    open_nodes
        .iter()
        .map(|open_node| {
            let (current_node_context_tokens, problem) =
                open_node_context_state(current, open_node);
            let node_id = open_node.node_id.to_string();
            let summary = snapshot_node_summary(snapshot, &node_id);
            SpineOpenNodeInside {
                node_id: open_node.node_id.clone(),
                summary,
                provider_input_tokens: open_node.provider_input_tokens,
                current_node_context_tokens,
                problem,
            }
        })
        .collect()
}

fn snapshot_node_summary(snapshot: &SpineTreeUpdateEvent, node_id: &str) -> Option<String> {
    snapshot
        .nodes
        .iter()
        .find(|node| node.node_id == node_id)
        .and_then(|node| node.summary.clone())
}

fn format_open_node_context_annotations(
    open_nodes: &[SpineOpenNodeInside],
) -> BTreeMap<NodeId, String> {
    open_nodes
        .iter()
        .filter_map(|open_node| {
            let tokens = open_node.current_node_context_tokens?;
            Some((
                open_node.node_id.clone(),
                format!("(~{} inclusive context)", format_si_suffix(tokens)),
            ))
        })
        .collect()
}

fn annotate_open_node_contexts(
    snapshot: &mut SpineTreeUpdateEvent,
    current: Option<&TokenUsageInfo>,
    open_nodes: &[OpenNodeContextProjection],
) {
    let open_nodes_by_id = open_nodes
        .iter()
        .map(|node| (node.node_id.to_string(), node))
        .collect::<BTreeMap<_, _>>();
    for node in &mut snapshot.nodes {
        let Some(open_node) = open_nodes_by_id.get(node.node_id.as_str()) else {
            continue;
        };
        let accounting = node
            .accounting
            .get_or_insert_with(SpineTreeNodeAccountingSnapshot::default);
        let (current_node_context_tokens, problem) = open_node_context_state(current, open_node);
        accounting.current_node_context_tokens = current_node_context_tokens;
        accounting.current_node_context_baseline_source = open_node.baseline_source;
        accounting.current_node_context_problem = problem;
    }
}

fn open_node_context_state(
    current: Option<&TokenUsageInfo>,
    open_node: &OpenNodeContextProjection,
) -> (Option<i64>, Option<SpineNodeContextProblem>) {
    if let Some(problem) = open_node.problem {
        return (None, Some(problem));
    }
    match node_context_tokens(current, open_node.provider_input_tokens) {
        Ok(tokens) => (Some(tokens), None),
        Err(problem) => (None, Some(problem)),
    }
}

fn context_window_inside(current: Option<&TokenUsageInfo>) -> Option<SpineContextWindowInside> {
    let current = current?;
    let context_tokens = current.last_token_usage.tokens_in_context_window();
    (context_tokens > 0).then_some(SpineContextWindowInside {
        context_tokens,
        model_context_window: current.model_context_window,
        remaining_percent: current.model_context_window.map(|window| {
            current
                .last_token_usage
                .percent_of_context_window_remaining(window)
        }),
    })
}

fn format_context_window_pressure(info: Option<&SpineContextWindowInside>) -> Option<String> {
    let info = info?;
    let window = info.model_context_window?;
    if window <= 0 {
        return None;
    }
    let used = info.context_tokens;
    if used <= 0 {
        return None;
    }
    let remaining = info.remaining_percent?.clamp(0, 100);
    Some(format!(
        "Context window: {remaining}% left ({} used / {})",
        format_si_suffix(used),
        format_si_suffix(window)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::protocol::TokenUsage;
    use codex_protocol::protocol::TokenUsageInfo;

    fn token_info(input_tokens: i64, total_tokens: i64) -> TokenUsageInfo {
        TokenUsageInfo {
            total_token_usage: TokenUsage::default(),
            last_token_usage: TokenUsage {
                input_tokens,
                total_tokens,
                ..TokenUsage::default()
            },
            model_context_window: None,
        }
    }

    #[test]
    fn node_context_tokens_supports_zero_delta() {
        let current = token_info(10_000, 10_000);
        assert_eq!(
            node_context_tokens(Some(&current), Some(10_000)).expect("delta"),
            0
        );
    }

    #[test]
    fn node_context_tokens_rejects_negative_delta() {
        let current = token_info(9_000, 9_000);
        assert_eq!(
            node_context_tokens(Some(&current), Some(10_000)).unwrap_err(),
            SpineNodeContextProblem::CoordinateMismatch
        );
    }
}
