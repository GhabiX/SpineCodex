use super::ids::NodeId;
use super::plan_bridge::PlanSnapshot;
use super::plan_bridge::PlanTreeScope;
use super::plan_bridge::PlanTreeSnapshot;
use super::runtime::SpineRuntimeError;
use super::state::NodeStatus;
use super::state::SpineState;
use super::store::SpineSidecarStore;
use super::view::display_node_id;
use codex_protocol::spine_tree::SpineTreeNodeSnapshot;
use codex_protocol::spine_tree::SpineTreeNodeStatus;
use codex_protocol::spine_tree::SpineTreePlanCheckpointSnapshot;
use codex_protocol::spine_tree::SpineTreePlanItemSnapshot;
use codex_protocol::spine_tree::SpineTreePlanItemStatus;
use codex_protocol::spine_tree::SpineTreePlanSnapshot;
use codex_protocol::spine_tree::SpineTreePlanTreeScopeSnapshot;
use codex_protocol::spine_tree::SpineTreePlanTreeSnapshot;
use codex_protocol::spine_tree::SpineTreeUpdateEvent;
use std::collections::HashSet;

pub(crate) fn build_tree_snapshot(
    state: &SpineState,
    store: &SpineSidecarStore,
    cursor: &NodeId,
    surviving_turn_ids: Option<&HashSet<String>>,
) -> Result<SpineTreeUpdateEvent, SpineRuntimeError> {
    let snapshot_seq = store.next_tree_event_seq()?.saturating_sub(1);
    let mut nodes = Vec::with_capacity(state.nodes().len());
    for (node_id, node) in state.nodes() {
        if node_id == &NodeId::root() {
            continue;
        }
        let plan = if node_id == cursor {
            store
                .read_projected_plan_snapshot(node_id, surviving_turn_ids)?
                .map(spine_tree_plan_snapshot)
                .transpose()?
        } else {
            None
        };
        nodes.push(SpineTreeNodeSnapshot {
            node_id: display_node_id(&node.node_id),
            parent_id: visible_parent_id(node.parent_id.as_ref()),
            summary: node.summary.clone(),
            status: match node.status {
                NodeStatus::Live => SpineTreeNodeStatus::Live,
                NodeStatus::Opened => SpineTreeNodeStatus::Opened,
                NodeStatus::Finished => SpineTreeNodeStatus::Finished,
                NodeStatus::Closed => SpineTreeNodeStatus::Closed,
            },
            plan,
        });
    }

    Ok(SpineTreeUpdateEvent {
        snapshot_seq,
        active_node_id: display_node_id(cursor),
        nodes,
    })
}

fn visible_parent_id(parent_id: Option<&NodeId>) -> Option<String> {
    match parent_id {
        Some(parent) if parent == &NodeId::root() => None,
        Some(parent) => Some(display_node_id(parent)),
        None => None,
    }
}

fn spine_tree_plan_snapshot(
    snapshot: PlanSnapshot,
) -> Result<SpineTreePlanSnapshot, SpineRuntimeError> {
    Ok(SpineTreePlanSnapshot {
        revision: snapshot.revision,
        explanation: snapshot.explanation,
        spine_plantree: snapshot.spine_plantree.map(spine_tree_plantree_snapshot),
        items: snapshot
            .items
            .into_iter()
            .map(|item| {
                let status = match item.status.as_str() {
                    "pending" => SpineTreePlanItemStatus::Pending,
                    "in_progress" => SpineTreePlanItemStatus::InProgress,
                    "completed" => SpineTreePlanItemStatus::Completed,
                    _ => {
                        return Err(SpineRuntimeError::UnknownPlanItemStatus(item.status));
                    }
                };
                Ok(SpineTreePlanItemSnapshot {
                    stable_task_id: item.stable_task_id,
                    step: item.step,
                    status,
                })
            })
            .collect::<Result<Vec<_>, _>>()?,
    })
}

fn spine_tree_plantree_snapshot(snapshot: PlanTreeSnapshot) -> SpineTreePlanTreeSnapshot {
    SpineTreePlanTreeSnapshot {
        anchor_node_id: display_node_id_from_str(&snapshot.anchor_node_id),
        root: spine_tree_plantree_scope_snapshot(snapshot.root),
    }
}

fn spine_tree_plantree_scope_snapshot(scope: PlanTreeScope) -> SpineTreePlanTreeScopeSnapshot {
    SpineTreePlanTreeScopeSnapshot {
        existing_node_id: scope
            .existing_node_id
            .as_deref()
            .map(display_node_id_from_str),
        summary: scope.summary,
        status: scope.status.and_then(spine_tree_plan_item_status),
        checkpoints: scope
            .checkpoints
            .into_iter()
            .filter_map(|checkpoint| {
                Some(SpineTreePlanCheckpointSnapshot {
                    task: checkpoint.task,
                    status: spine_tree_plan_item_status(checkpoint.status)?,
                })
            })
            .collect(),
        children: scope
            .children
            .into_iter()
            .map(spine_tree_plantree_scope_snapshot)
            .collect(),
    }
}

fn display_node_id_from_str(node_id: &str) -> String {
    NodeId::parse(node_id)
        .map(|node_id| display_node_id(&node_id))
        .unwrap_or_else(|_| node_id.to_string())
}

fn spine_tree_plan_item_status(status: impl AsRef<str>) -> Option<SpineTreePlanItemStatus> {
    match status.as_ref() {
        "pending" => Some(SpineTreePlanItemStatus::Pending),
        "in_progress" => Some(SpineTreePlanItemStatus::InProgress),
        "completed" => Some(SpineTreePlanItemStatus::Completed),
        _ => None,
    }
}
