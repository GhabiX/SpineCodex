//! Host-neutral status facts; terminal and protocol rendering stay host-owned.

use crate::NodeId;
use crate::NodeStatus;
use crate::RawBoundary;
use crate::SpineProjection;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeSnapshot {
    pub cursor: NodeId,
    pub nodes: Vec<TreeNode>,
    pub last_boundary: Option<RawBoundary>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeNode {
    pub id: NodeId,
    pub parent: Option<NodeId>,
    pub kind: crate::NodeKind,
    pub status: NodeStatus,
    pub summary: Option<String>,
    pub memory_summary: Option<String>,
    pub spawn_outcome: Option<crate::SpawnOutcome>,
    pub start: RawBoundary,
    pub end: Option<RawBoundary>,
    pub pressure: Option<ContextPressure>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextPressure {
    pub open_input_tokens: Option<i64>,
    pub current_input_tokens: Option<i64>,
    pub context_tokens: Option<i64>,
    pub problem: Option<ContextPressureProblem>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextPressureProblem {
    MissingCurrentUsage,
    MissingOpenContextBaseline,
    CoordinateMismatch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenUsageSample {
    pub boundary: RawBoundary,
    pub input_tokens: i64,
}

/// Effective model context capacity observed at a source boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextWindowSample {
    pub boundary: RawBoundary,
    pub model_context_window: i64,
}

/// Model-visible cost of the context inherited when a Spine task node opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeContextCost {
    Percentage(u64),
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusSignal {
    pub cursor: NodeId,
    pub node_summary: Option<String>,
    pub parent: Option<NodeId>,
    pub parent_summary: Option<String>,
    pub cursor_node_context_tokens: Option<i64>,
    pub context_left_tokens: Option<i64>,
}

pub fn status_signal(
    projection: &SpineProjection,
    pressures: &BTreeMap<NodeId, ContextPressure>,
    context_left_tokens: Option<i64>,
) -> StatusSignal {
    let active_node = projection
        .nodes
        .iter()
        .find(|node| node.id == projection.cursor);
    let parent = active_node.and_then(|node| node.parent.clone());
    let parent_summary = parent.as_ref().and_then(|parent_id| {
        projection
            .nodes
            .iter()
            .find(|node| &node.id == parent_id)
            .and_then(|node| node.summary.clone())
    });
    StatusSignal {
        cursor: projection.cursor.clone(),
        node_summary: active_node.and_then(|node| node.summary.clone()),
        parent,
        parent_summary,
        cursor_node_context_tokens: pressures
            .get(&projection.cursor)
            .and_then(|pressure| pressure.context_tokens),
        context_left_tokens,
    }
}

pub fn tree_snapshot(projection: &SpineProjection, samples: &[TokenUsageSample]) -> TreeSnapshot {
    let pressures = context_pressures(projection, samples);
    let nodes = projection
        .nodes
        .iter()
        .map(|node| TreeNode {
            id: node.id.clone(),
            parent: node.parent.clone(),
            kind: node.kind,
            status: node.status,
            summary: node.summary.clone(),
            memory_summary: node.memory.as_ref().and_then(|slots| {
                slots.iter().rev().find_map(|slot| match slot {
                    crate::MemorySlot::Summary { body, .. } => Some(body.clone()),
                    _ => None,
                })
            }),
            spawn_outcome: node.memory.as_ref().and_then(|slots| {
                slots.iter().find_map(|slot| match slot {
                    crate::MemorySlot::SpawnEvidence {
                        owner_node,
                        outcome,
                        ..
                    } if owner_node == &node.id => Some(*outcome),
                    _ => None,
                })
            }),
            start: node.start,
            end: node.end,
            pressure: pressures.get(&node.id).cloned(),
        })
        .collect();
    TreeSnapshot {
        cursor: projection.cursor.clone(),
        nodes,
        last_boundary: projection.last_boundary,
    }
}

pub fn context_pressures(
    projection: &SpineProjection,
    samples: &[TokenUsageSample],
) -> BTreeMap<NodeId, ContextPressure> {
    let current = samples
        .iter()
        .rev()
        .find_map(|sample| (sample.input_tokens > 0).then_some(sample.input_tokens));
    projection
        .nodes
        .iter()
        .filter(|node| matches!(node.status, NodeStatus::Live | NodeStatus::Opened))
        .map(|node| {
            let open = samples
                .iter()
                .find(|sample| sample.boundary.0 > node.start.0)
                .map(|sample| sample.input_tokens)
                .filter(|tokens| *tokens > 0);
            let (context_tokens, problem) = match (current, open) {
                (None, _) => (None, Some(ContextPressureProblem::MissingCurrentUsage)),
                (Some(_), None) => (
                    None,
                    Some(ContextPressureProblem::MissingOpenContextBaseline),
                ),
                (Some(current), Some(open)) => match current.checked_sub(open) {
                    Some(tokens) if tokens >= 0 => (Some(tokens), None),
                    _ => (None, Some(ContextPressureProblem::CoordinateMismatch)),
                },
            };
            (
                node.id.clone(),
                ContextPressure {
                    open_input_tokens: open,
                    current_input_tokens: current,
                    context_tokens,
                    problem,
                },
            )
        })
        .collect()
}

pub(crate) fn context_cost(
    open_input_tokens: Option<u64>,
    model_context_window: i64,
) -> NodeContextCost {
    let Some(input_tokens) = open_input_tokens else {
        return NodeContextCost::Unavailable;
    };
    let Some(context_window) = u64::try_from(model_context_window)
        .ok()
        .filter(|tokens| *tokens > 0)
    else {
        return NodeContextCost::Unavailable;
    };
    let percent = u128::from(input_tokens)
        .saturating_mul(100)
        .div_ceil(u128::from(context_window));
    NodeContextCost::Percentage(u64::try_from(percent).unwrap_or(u64::MAX))
}

#[cfg(test)]
#[path = "status_tests.rs"]
mod tests;
