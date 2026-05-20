use super::*;
use pretty_assertions::assert_eq;

fn id(segments: &[u32]) -> NodeId {
    NodeId::from_segments(segments.to_vec())
}

fn summaries(state: &SpineState) -> Vec<(NodeId, Option<String>, NodeStatus)> {
    state
        .nodes()
        .values()
        .map(|node| {
            (
                node.node_id.clone(),
                node.summary.clone(),
                node.status.clone(),
            )
        })
        .collect()
}

fn assert_tree_invariants(state: &SpineState) {
    let live_nodes = state
        .nodes()
        .values()
        .filter(|node| node.status == NodeStatus::Live)
        .map(|node| node.node_id.clone())
        .collect::<Vec<_>>();
    assert_eq!(live_nodes, vec![state.cursor().clone()]);

    for node in state.nodes().values() {
        if node.status == NodeStatus::Closed {
            for descendant in state.nodes().values() {
                if is_descendant(state, &descendant.node_id, &node.node_id) {
                    assert!(
                        descendant.status == NodeStatus::Closed,
                        "closed node {} has unfinished descendant {} with status {:?}",
                        node.node_id,
                        descendant.node_id,
                        descendant.status
                    );
                }
            }
        }

        if node.status == NodeStatus::Suspended {
            assert!(
                is_ancestor(state, &node.node_id, state.cursor()),
                "suspended node {} is not on cursor path {}",
                node.node_id,
                state.cursor()
            );
        }
    }
}

fn is_descendant(state: &SpineState, node_id: &NodeId, ancestor_id: &NodeId) -> bool {
    let mut parent_id = state.node(node_id).and_then(|node| node.parent_id.as_ref());
    while let Some(parent) = parent_id {
        if parent == ancestor_id {
            return true;
        }
        parent_id = state.node(parent).and_then(|node| node.parent_id.as_ref());
    }
    false
}

fn is_ancestor(state: &SpineState, ancestor_id: &NodeId, node_id: &NodeId) -> bool {
    ancestor_id == node_id || is_descendant(state, node_id, ancestor_id)
}

#[test]
fn initializes_root_with_initial_leaf() {
    let state = SpineState::new();

    assert_eq!(state.cursor(), &id(&[1, 1]));
    assert_eq!(
        summaries(&state),
        vec![
            (id(&[1]), None, NodeStatus::Suspended),
            (id(&[1, 1]), None, NodeStatus::Live),
        ]
    );
    assert_eq!(state.visible_spine(), vec![id(&[1]), id(&[1, 1])]);
}

#[test]
fn open_enters_first_child_without_summary() {
    let mut state = SpineState::new();

    let transition = state.open().expect("open should succeed");

    assert_eq!(
        transition,
        Transition {
            from: id(&[1, 1]),
            to: id(&[1, 1, 1]),
        }
    );
    assert_eq!(state.cursor(), &id(&[1, 1, 1]));
    assert_eq!(
        summaries(&state),
        vec![
            (id(&[1]), None, NodeStatus::Suspended),
            (id(&[1, 1]), None, NodeStatus::Suspended),
            (id(&[1, 1, 1]), None, NodeStatus::Live),
        ]
    );
}

#[test]
fn close_then_open_enters_next_sibling() {
    let mut state = SpineState::new();

    let close = state
        .close("first child done")
        .expect("close should succeed");
    let open = state.open().expect("open sibling should succeed");

    assert_eq!(
        close,
        Transition {
            from: id(&[1, 1]),
            to: id(&[1]),
        }
    );
    assert_eq!(
        open,
        Transition {
            from: id(&[1]),
            to: id(&[1, 2]),
        }
    );
    assert_eq!(state.cursor(), &id(&[1, 2]));
    assert_eq!(
        summaries(&state),
        vec![
            (id(&[1]), None, NodeStatus::Suspended),
            (
                id(&[1, 1]),
                Some("first child done".to_string()),
                NodeStatus::Closed,
            ),
            (id(&[1, 2]), None, NodeStatus::Live),
        ]
    );
    assert_eq!(
        state.visible_spine(),
        vec![id(&[1]), id(&[1, 1]), id(&[1, 2])]
    );
}

#[test]
fn repeated_close_open_allocates_consecutive_siblings() {
    let mut state = SpineState::new();
    state
        .close("first child done")
        .expect("first close should succeed");
    state.open().expect("second sibling open should succeed");

    state
        .close("second child done")
        .expect("second close should succeed");
    let transition = state.open().expect("third sibling open should succeed");

    assert_eq!(
        transition,
        Transition {
            from: id(&[1]),
            to: id(&[1, 3]),
        }
    );
    assert_eq!(
        state.visible_spine(),
        vec![id(&[1]), id(&[1, 1]), id(&[1, 2]), id(&[1, 3])]
    );
}

#[test]
fn close_root_child_returns_to_root_epoch() {
    let mut state = SpineState::new();

    let transition = state
        .close("root child done")
        .expect("close root child should return to root epoch");

    assert_eq!(
        transition,
        Transition {
            from: id(&[1, 1]),
            to: id(&[1]),
        }
    );
    assert_eq!(state.cursor(), &id(&[1]));
    assert_eq!(
        summaries(&state),
        vec![
            (id(&[1]), None, NodeStatus::Live),
            (
                id(&[1, 1]),
                Some("root child done".to_string()),
                NodeStatus::Closed,
            ),
        ]
    );
    assert_eq!(state.visible_spine(), vec![id(&[1]), id(&[1, 1])]);
}

#[test]
fn deep_close_returns_to_parent_with_closed_child_visible() {
    let mut state = SpineState::new();
    state.open().expect("nested open should succeed");

    let transition = state
        .close("nested scope done")
        .expect("deep close should succeed");

    assert_eq!(
        transition,
        Transition {
            from: id(&[1, 1, 1]),
            to: id(&[1, 1]),
        }
    );
    assert_eq!(state.cursor(), &id(&[1, 1]));
    assert_eq!(
        state.node(&id(&[1, 1])).map(|node| node.status.clone()),
        Some(NodeStatus::Live)
    );
    assert_eq!(
        state
            .node(&id(&[1, 1]))
            .and_then(|node| node.summary.clone()),
        None
    );
    assert_eq!(
        state.node(&id(&[1, 1, 1])).map(|node| node.status.clone()),
        Some(NodeStatus::Closed)
    );
    assert_eq!(
        state.visible_spine(),
        vec![id(&[1]), id(&[1, 1]), id(&[1, 1, 1])]
    );
}

#[test]
fn close_on_root_fails_without_mutating_state() {
    let mut state = SpineState::from_records(
        id(&[1]),
        vec![NodeRecord {
            node_id: id(&[1]),
            parent_id: None,
            raw_start_ordinal: Some(0),
            status: NodeStatus::Live,
            summary: None,
        }],
    )
    .expect("construct root cursor state");
    let before = state.clone();

    let error = state
        .close("root done")
        .expect_err("root close should fail");

    assert_eq!(error, SpineStateError::CannotCloseRoot);
    assert_eq!(state, before);
}

#[test]
fn from_records_rejects_multiple_live_nodes() {
    let error = SpineState::from_records(
        id(&[1, 1]),
        vec![
            NodeRecord {
                node_id: id(&[1]),
                parent_id: None,
                raw_start_ordinal: Some(0),
                status: NodeStatus::Suspended,
                summary: None,
            },
            NodeRecord {
                node_id: id(&[1, 1]),
                parent_id: Some(id(&[1])),
                raw_start_ordinal: Some(0),
                status: NodeStatus::Live,
                summary: None,
            },
            NodeRecord {
                node_id: id(&[1, 2]),
                parent_id: Some(id(&[1])),
                raw_start_ordinal: Some(0),
                status: NodeStatus::Live,
                summary: None,
            },
        ],
    )
    .expect_err("multiple live nodes should fail closed");

    assert!(matches!(error, SpineStateError::MultipleLiveNodes { .. }));
}

#[test]
fn from_records_rejects_suspended_node_outside_cursor_path() {
    let error = SpineState::from_records(
        id(&[1, 1]),
        vec![
            NodeRecord {
                node_id: id(&[1]),
                parent_id: None,
                raw_start_ordinal: Some(0),
                status: NodeStatus::Suspended,
                summary: None,
            },
            NodeRecord {
                node_id: id(&[1, 1]),
                parent_id: Some(id(&[1])),
                raw_start_ordinal: Some(0),
                status: NodeStatus::Live,
                summary: None,
            },
            NodeRecord {
                node_id: id(&[1, 2]),
                parent_id: Some(id(&[1])),
                raw_start_ordinal: Some(0),
                status: NodeStatus::Suspended,
                summary: None,
            },
        ],
    )
    .expect_err("suspended non-ancestor should fail closed");

    assert!(matches!(
        error,
        SpineStateError::SuspendedNodeOutsideCursorPath { .. }
    ));
}

#[test]
fn from_records_rejects_unfinished_descendant_under_closed_node() {
    let error = SpineState::from_records(
        id(&[2, 1]),
        vec![
            NodeRecord {
                node_id: id(&[1]),
                parent_id: None,
                raw_start_ordinal: Some(0),
                status: NodeStatus::Closed,
                summary: Some("Context compacted".to_string()),
            },
            NodeRecord {
                node_id: id(&[1, 1]),
                parent_id: Some(id(&[1])),
                raw_start_ordinal: Some(0),
                status: NodeStatus::Suspended,
                summary: None,
            },
            NodeRecord {
                node_id: id(&[2]),
                parent_id: None,
                raw_start_ordinal: Some(7),
                status: NodeStatus::Suspended,
                summary: None,
            },
            NodeRecord {
                node_id: id(&[2, 1]),
                parent_id: Some(id(&[2])),
                raw_start_ordinal: Some(7),
                status: NodeStatus::Live,
                summary: None,
            },
        ],
    )
    .expect_err("unfinished descendant under closed node should fail closed");

    assert!(matches!(
        error,
        SpineStateError::ClosedNodeHasUnfinishedDescendant { .. }
    ));
}

#[test]
fn reset_root_epoch_replaces_live_tree_under_stable_root() {
    let mut state = SpineState::new();
    state.open().expect("nested open should succeed");

    let transition = state
        .reset_root_epoch("context compacted", 21)
        .expect("reset root epoch should succeed");

    assert_eq!(
        transition,
        Transition {
            from: id(&[1]),
            to: id(&[2, 1]),
        }
    );
    assert_eq!(state.cursor(), &id(&[2, 1]));
    assert_eq!(
        summaries(&state),
        vec![
            (
                id(&[1]),
                Some("context compacted".to_string()),
                NodeStatus::Closed,
            ),
            (id(&[1, 1]), None, NodeStatus::Closed),
            (id(&[1, 1, 1]), None, NodeStatus::Closed),
            (id(&[2]), None, NodeStatus::Suspended),
            (id(&[2, 1]), None, NodeStatus::Live),
        ]
    );
    assert_tree_invariants(&state);
    assert_eq!(
        state
            .node(&id(&[2, 1]))
            .and_then(|node| node.raw_start_ordinal),
        Some(21)
    );

    let transition = state
        .reset_root_epoch("context compacted again", 34)
        .expect("second reset root epoch should succeed");
    assert_eq!(
        transition,
        Transition {
            from: id(&[2]),
            to: id(&[3, 1]),
        }
    );
    assert_eq!(state.cursor(), &id(&[3, 1]));
    assert_eq!(
        state.node(&id(&[2])).and_then(|node| node.summary.clone()),
        Some("context compacted again".to_string())
    );
    assert_tree_invariants(&state);
}

#[test]
fn empty_summary_fails_without_mutating_state() {
    let mut state = SpineState::new();
    let before = state.clone();

    let empty_summary = state.close(" ").expect_err("empty summary should fail");
    assert_eq!(empty_summary, SpineStateError::EmptySummary);
    assert_eq!(state, before);
}

#[test]
fn visible_spine_excludes_left_sibling_descendants() {
    let mut state = SpineState::new();
    state.open().expect("nested open should succeed");
    state
        .close("nested done")
        .expect("nested close should succeed");
    state
        .close("first child done")
        .expect("close first child should succeed");
    state.open().expect("second child open should succeed");

    assert_eq!(state.cursor(), &id(&[1, 2]));
    assert_eq!(
        state.visible_spine(),
        vec![id(&[1]), id(&[1, 1]), id(&[1, 2])]
    );
    assert!(state.node(&id(&[1, 1, 1])).is_some());
}
