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

#[test]
fn initializes_root_node() {
    let state = SpineState::new();

    assert_eq!(state.cursor(), &id(&[1]));
    assert_eq!(summaries(&state), vec![(id(&[1]), None, NodeStatus::Live)]);
    assert_eq!(state.visible_spine(), vec![id(&[1])]);
}

#[test]
fn open_writes_summary_and_enters_first_child() {
    let mut state = SpineState::new();

    let transition = state.open("root scope").expect("open should succeed");

    assert_eq!(
        transition,
        Transition {
            from: id(&[1]),
            to: id(&[1, 1]),
        }
    );
    assert_eq!(state.cursor(), &id(&[1, 1]));
    assert_eq!(
        summaries(&state),
        vec![
            (id(&[1]), Some("root scope".to_string()), NodeStatus::Opened,),
            (id(&[1, 1]), None, NodeStatus::Live),
        ]
    );
    assert_eq!(state.visible_spine(), vec![id(&[1]), id(&[1, 1])]);
}

#[test]
fn next_finishes_leaf_and_enters_next_sibling() {
    let mut state = SpineState::new();
    state.open("root scope").expect("open should succeed");

    let transition = state.next("first child done").expect("next should succeed");

    assert_eq!(
        transition,
        Transition {
            from: id(&[1, 1]),
            to: id(&[1, 2]),
        }
    );
    assert_eq!(state.cursor(), &id(&[1, 2]));
    assert_eq!(
        summaries(&state),
        vec![
            (id(&[1]), Some("root scope".to_string()), NodeStatus::Opened,),
            (
                id(&[1, 1]),
                Some("first child done".to_string()),
                NodeStatus::Finished,
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
fn next_on_root_finishes_root_and_enters_next_root_sibling() {
    let mut state = SpineState::new();

    let transition = state.next("root done").expect("root next should succeed");

    assert_eq!(
        transition,
        Transition {
            from: id(&[1]),
            to: id(&[2]),
        }
    );
    assert_eq!(state.cursor(), &id(&[2]));
    assert_eq!(
        summaries(&state),
        vec![
            (
                id(&[1]),
                Some("root done".to_string()),
                NodeStatus::Finished,
            ),
            (id(&[2]), None, NodeStatus::Live),
        ]
    );
    assert_eq!(state.visible_spine(), vec![id(&[1]), id(&[2])]);
}

#[test]
fn repeated_next_allocates_consecutive_siblings() {
    let mut state = SpineState::new();
    state.open("root scope").expect("open should succeed");
    state
        .next("first child done")
        .expect("first next should succeed");

    let transition = state
        .next("second child done")
        .expect("second next should succeed");

    assert_eq!(
        transition,
        Transition {
            from: id(&[1, 2]),
            to: id(&[1, 3]),
        }
    );
    assert_eq!(
        state.visible_spine(),
        vec![id(&[1]), id(&[1, 1]), id(&[1, 2]), id(&[1, 3])]
    );
}

#[test]
fn close_that_would_close_root_scope_fails_without_mutating_state() {
    let mut state = SpineState::new();
    state.open("root scope").expect("open should succeed");
    let before = state.clone();

    let error = state
        .close("child scope done")
        .expect_err("close should reject root scope");

    assert_eq!(error, SpineStateError::CannotCloseRoot);
    assert_eq!(state, before);
}

#[test]
fn deep_close_returns_to_parent_sibling() {
    let mut state = SpineState::new();
    state.open("root scope").expect("root open should succeed");
    state
        .open("child scope")
        .expect("child open should succeed");

    let transition = state
        .close("nested done")
        .expect("deep close should succeed");

    assert_eq!(
        transition,
        Transition {
            from: id(&[1, 1, 1]),
            to: id(&[1, 2]),
        }
    );
    assert_eq!(state.cursor(), &id(&[1, 2]));
    assert_eq!(
        state.node(&id(&[1, 1])).map(|node| node.status.clone()),
        Some(NodeStatus::Closed)
    );
    assert_eq!(
        state.visible_spine(),
        vec![id(&[1]), id(&[1, 1]), id(&[1, 2])]
    );
}

#[test]
fn close_on_root_fails_without_mutating_state() {
    let mut state = SpineState::new();
    let before = state.clone();

    let error = state
        .close("root done")
        .expect_err("root close should fail");

    assert_eq!(error, SpineStateError::CannotCloseRoot);
    assert_eq!(state, before);
}

#[test]
fn empty_summary_fails_without_mutating_state() {
    let mut state = SpineState::new();
    let before = state.clone();

    let empty_summary = state.open(" ").expect_err("empty summary should fail");
    assert_eq!(empty_summary, SpineStateError::EmptySummary);
    assert_eq!(state, before);
}

#[test]
fn visible_spine_excludes_left_sibling_descendants() {
    let mut state = SpineState::new();
    state.open("root scope").expect("open should succeed");
    state
        .open("first child scope")
        .expect("nested open should succeed");
    state
        .close("nested done")
        .expect("nested close should succeed");
    state
        .next("second child done")
        .expect("next should succeed");

    assert_eq!(state.cursor(), &id(&[1, 3]));
    assert_eq!(
        state.visible_spine(),
        vec![id(&[1]), id(&[1, 1]), id(&[1, 2]), id(&[1, 3])]
    );
    assert!(state.node(&id(&[1, 1, 1])).is_some());
}
