use crate::spine::NodeId;
use crate::spine::SpineError;
use crate::spine::SpineOpenNodeContextProjection;
use crate::spine::SpineRuntime;
use codex_protocol::num_format::format_si_suffix;
use codex_protocol::protocol::TokenUsageInfo;
use codex_protocol::spine_tree::SpineNodeContextUnavailableReason;
use codex_protocol::spine_tree::SpineTreeNodeAccountingSnapshot;
use codex_protocol::spine_tree::SpineTreeUpdateEvent;
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub(crate) struct SpineTreeInsideView {
    pub(crate) active_node_id: String,
    pub(crate) active_node_summary: Option<String>,
    pub(crate) rendered_tree: String,
    pub(crate) snapshot: SpineTreeUpdateEvent,
    pub(crate) open_nodes: Vec<SpineOpenNodeInside>,
    pub(crate) context_window: Option<SpineContextWindowInside>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpineOpenNodeInside {
    pub(crate) node_id: NodeId,
    pub(crate) summary: Option<String>,
    pub(crate) baseline_tokens: Option<i64>,
    pub(crate) current_node_context_tokens: Option<i64>,
    pub(crate) unavailable: Option<SpineNodeContextUnavailableReason>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpineContextWindowInside {
    pub(crate) context_tokens: i64,
    pub(crate) model_context_window: Option<i64>,
    remaining_percent: Option<i64>,
}

pub(crate) fn build_spine_tree_inside_view(
    runtime: &SpineRuntime,
    token_info: Option<&TokenUsageInfo>,
) -> Result<SpineTreeInsideView, SpineError> {
    let open_node_projections = runtime.open_node_context_projections();
    let mut snapshot = runtime.build_tree_snapshot()?;
    annotate_open_node_contexts(&mut snapshot, token_info, &open_node_projections);

    let active_node_id = snapshot.active_node_id.clone();
    let active_node_summary = snapshot
        .nodes
        .iter()
        .find(|node| node.node_id == snapshot.active_node_id)
        .and_then(|node| node.summary.clone());

    let open_nodes = build_open_nodes_inside(&snapshot, token_info, &open_node_projections);
    let annotations = format_open_node_context_annotations(&open_nodes);
    let mut rendered_tree = runtime.render_tree_with_context_annotations(&annotations)?;
    let context_window = context_window_inside(token_info);
    if let Some(line) = format_context_window_pressure(context_window.as_ref()) {
        rendered_tree.push_str("\n\n");
        rendered_tree.push_str(&line);
    }

    Ok(SpineTreeInsideView {
        active_node_id,
        active_node_summary,
        rendered_tree,
        snapshot,
        open_nodes,
        context_window,
    })
}

pub(crate) fn node_context_tokens(
    current: Option<&TokenUsageInfo>,
    open_context_tokens: Option<i64>,
) -> Result<Option<i64>, SpineNodeContextUnavailableReason> {
    let current = current
        .ok_or(SpineNodeContextUnavailableReason::MissingCurrentUsage)?
        .last_token_usage
        .tokens_in_context_window();
    let open_context_tokens =
        open_context_tokens.ok_or(SpineNodeContextUnavailableReason::MissingOpenContextBaseline)?;
    let tokens = current.saturating_sub(open_context_tokens);
    Ok((tokens > 0).then_some(tokens))
}

pub(crate) fn context_unavailable_reason_label(
    reason: SpineNodeContextUnavailableReason,
) -> &'static str {
    match reason {
        SpineNodeContextUnavailableReason::MissingCurrentUsage => "missing current usage",
        SpineNodeContextUnavailableReason::MissingOpenContextBaseline => "missing open baseline",
        SpineNodeContextUnavailableReason::NonPositiveDelta => "non-positive delta",
        SpineNodeContextUnavailableReason::CorruptPressureMetadata => "corrupt pressure metadata",
    }
}

fn build_open_nodes_inside(
    snapshot: &SpineTreeUpdateEvent,
    current: Option<&TokenUsageInfo>,
    open_nodes: &[SpineOpenNodeContextProjection],
) -> Vec<SpineOpenNodeInside> {
    open_nodes
        .iter()
        .map(|open_node| {
            let (current_node_context_tokens, unavailable) =
                match node_context_tokens(current, open_node.baseline_tokens) {
                    Ok(Some(tokens)) => (Some(tokens), None),
                    Ok(None) => (
                        None,
                        Some(SpineNodeContextUnavailableReason::NonPositiveDelta),
                    ),
                    Err(reason) => (None, Some(reason)),
                };
            let node_id = open_node.node_id.to_string();
            let summary = snapshot
                .nodes
                .iter()
                .find(|node| node.node_id == node_id)
                .and_then(|node| node.summary.clone());
            SpineOpenNodeInside {
                node_id: open_node.node_id.clone(),
                summary,
                baseline_tokens: open_node.baseline_tokens,
                current_node_context_tokens,
                unavailable,
            }
        })
        .collect()
}

fn format_open_node_context_annotations(
    open_nodes: &[SpineOpenNodeInside],
) -> BTreeMap<NodeId, String> {
    open_nodes
        .iter()
        .map(|open_node| {
            let annotation = if let Some(tokens) = open_node.current_node_context_tokens {
                format!("(~{} inclusive context)", format_si_suffix(tokens))
            } else if let Some(reason) = open_node.unavailable {
                format!(
                    "(context unavailable: {})",
                    context_unavailable_reason_label(reason)
                )
            } else {
                "(context unavailable: non-positive delta)".to_string()
            };
            (open_node.node_id.clone(), annotation)
        })
        .collect()
}

fn annotate_open_node_contexts(
    snapshot: &mut SpineTreeUpdateEvent,
    current: Option<&TokenUsageInfo>,
    open_nodes: &[SpineOpenNodeContextProjection],
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
        match node_context_tokens(current, open_node.baseline_tokens) {
            Ok(Some(tokens)) => {
                accounting.current_node_context_tokens = Some(tokens);
                accounting.current_node_context_baseline_source = open_node.baseline_source;
                accounting.current_node_context_unavailable = None;
            }
            Ok(None) => {
                accounting.current_node_context_tokens = None;
                accounting.current_node_context_baseline_source = open_node.baseline_source;
                accounting.current_node_context_unavailable =
                    Some(SpineNodeContextUnavailableReason::NonPositiveDelta);
            }
            Err(reason) => {
                accounting.current_node_context_tokens = None;
                accounting.current_node_context_baseline_source = open_node.baseline_source;
                accounting.current_node_context_unavailable = Some(reason);
            }
        }
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
