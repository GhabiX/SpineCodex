use super::ids::NodeId;
use super::runtime::SpineRuntimeError;
use super::state::NodeStatus;
use super::state::SpineState;
use super::store::SpineSidecarStore;
use super::view::display_node_id;
use codex_protocol::spine_tree::SpineTreeNodeSnapshot;
use codex_protocol::spine_tree::SpineTreeNodeStatus;
use codex_protocol::spine_tree::SpineTreeUpdateEvent;

pub(crate) fn build_tree_snapshot(
    state: &SpineState,
    store: &SpineSidecarStore,
    cursor: &NodeId,
) -> Result<SpineTreeUpdateEvent, SpineRuntimeError> {
    let snapshot_seq = store.next_tree_event_seq()?.saturating_sub(1);
    let mut nodes = Vec::with_capacity(state.nodes().len());
    for (node_id, node) in state.nodes() {
        if node_id == &NodeId::root() {
            continue;
        }
        nodes.push(SpineTreeNodeSnapshot {
            node_id: display_node_id(&node.node_id),
            parent_id: visible_parent_id(node.parent_id.as_ref()),
            summary: node.summary.clone(),
            status: match node.status {
                NodeStatus::Live => SpineTreeNodeStatus::Live,
                NodeStatus::Suspended => SpineTreeNodeStatus::Suspended,
                NodeStatus::Closed => SpineTreeNodeStatus::Closed,
            },
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
