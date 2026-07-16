use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::RolloutItem;
use codex_spine_core::NodeId;
use codex_spine_core::NodeStatus;
use codex_spine_core::RawBoundary;
use codex_spine_core::SpineProjection;
use std::collections::BTreeMap;

use super::effective_rollout;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NodeContextPressure {
    pub(crate) open_input_tokens: Option<i64>,
    pub(crate) current_input_tokens: Option<i64>,
    pub(crate) context_tokens: Option<i64>,
    pub(crate) problem: Option<NodeContextPressureProblem>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NodeContextPressureProblem {
    MissingCurrentUsage,
    MissingOpenContextBaseline,
    CoordinateMismatch,
}

pub(crate) fn project(
    rollout: &[RolloutItem],
    projection: &SpineProjection,
) -> BTreeMap<NodeId, NodeContextPressure> {
    let effective = effective_rollout(rollout);
    project_from_effective(&effective, projection)
}

pub(super) fn project_from_effective(
    effective_rollout: &[(usize, &RolloutItem)],
    projection: &SpineProjection,
) -> BTreeMap<NodeId, NodeContextPressure> {
    let current_input_tokens = effective_rollout
        .iter()
        .rev()
        .find_map(|(_, item)| provider_input_tokens(item));

    projection
        .nodes
        .iter()
        .filter(|node| matches!(node.status, NodeStatus::Live | NodeStatus::Opened))
        .map(|node| {
            let open_input_tokens = provider_input_baseline_after(effective_rollout, node.start);
            let (context_tokens, problem) = context_state(current_input_tokens, open_input_tokens);
            (
                node.id.clone(),
                NodeContextPressure {
                    open_input_tokens,
                    current_input_tokens,
                    context_tokens,
                    problem,
                },
            )
        })
        .collect()
}

fn provider_input_baseline_after(
    effective_rollout: &[(usize, &RolloutItem)],
    open_boundary: RawBoundary,
) -> Option<i64> {
    effective_rollout.iter().find_map(|(boundary, item)| {
        let boundary = u64::try_from(*boundary).unwrap_or(u64::MAX);
        (boundary > open_boundary.0)
            .then(|| provider_input_tokens(item))
            .flatten()
    })
}

fn provider_input_tokens(item: &RolloutItem) -> Option<i64> {
    let RolloutItem::EventMsg(EventMsg::TokenCount(event)) = item else {
        return None;
    };
    let input_tokens = event.info.as_ref()?.last_token_usage.input_tokens;
    (input_tokens > 0).then_some(input_tokens)
}

fn context_state(
    current_input_tokens: Option<i64>,
    open_input_tokens: Option<i64>,
) -> (Option<i64>, Option<NodeContextPressureProblem>) {
    let Some(current) = current_input_tokens else {
        return (None, Some(NodeContextPressureProblem::MissingCurrentUsage));
    };
    let Some(open) = open_input_tokens else {
        return (
            None,
            Some(NodeContextPressureProblem::MissingOpenContextBaseline),
        );
    };
    match current.checked_sub(open) {
        Some(tokens) if tokens >= 0 => (Some(tokens), None),
        Some(_) | None => (None, Some(NodeContextPressureProblem::CoordinateMismatch)),
    }
}
