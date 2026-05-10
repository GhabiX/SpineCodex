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
fn open_writes_current_worklog_and_enters_first_child() {
    let mut state = SpineState::new();

    let transition = state
        .open("root scope", "Root handoff for child work.")
        .expect("open should succeed");

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
    assert_eq!(
        state
            .node(&id(&[1]))
            .and_then(|node| node.worklog.as_deref()),
        Some("Root handoff for child work.")
    );
}

#[test]
fn next_finishes_leaf_and_enters_next_sibling() {
    let mut state = SpineState::new();
    state
        .open("root scope", "Root handoff for child work.")
        .expect("open should succeed");

    let transition = state
        .next("first child done", "Child handoff for sibling work.")
        .expect("next should succeed");

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

    let transition = state
        .next("root done", "Root top-level work finished.")
        .expect("root next should succeed");

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
    state
        .open("root scope", "Root handoff for child work.")
        .expect("open should succeed");
    state
        .next("first child done", "First child handoff.")
        .expect("first next should succeed");

    let transition = state
        .next("second child done", "Second child handoff.")
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
fn close_finishes_leaf_closes_parent_and_enters_parent_sibling() {
    let mut state = SpineState::new();
    state
        .open("root scope", "Root handoff for child work.")
        .expect("open should succeed");

    let transition = state
        .close("child scope done", "Child handoff back to top level.")
        .expect("close should succeed");

    assert_eq!(
        transition,
        Transition {
            from: id(&[1, 1]),
            to: id(&[2]),
        }
    );
    assert_eq!(state.cursor(), &id(&[2]));
    assert_eq!(
        summaries(&state),
        vec![
            (id(&[1]), Some("root scope".to_string()), NodeStatus::Closed,),
            (
                id(&[1, 1]),
                Some("child scope done".to_string()),
                NodeStatus::Finished,
            ),
            (id(&[2]), None, NodeStatus::Live),
        ]
    );
    assert_eq!(state.visible_spine(), vec![id(&[1]), id(&[2])]);
}

#[test]
fn deep_close_returns_to_parent_sibling() {
    let mut state = SpineState::new();
    state
        .open("root scope", "Root handoff for child work.")
        .expect("root open should succeed");
    state
        .open("child scope", "Child handoff for nested work.")
        .expect("child open should succeed");

    let transition = state
        .close("nested done", "Nested handoff to parent sibling.")
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
        .close("root done", "Root cannot be closed.")
        .expect_err("root close should fail");

    assert_eq!(error, SpineStateError::CannotCloseRoot);
    assert_eq!(state, before);
}

#[test]
fn empty_summary_and_worklog_fail_without_mutating_state() {
    let mut state = SpineState::new();
    let before = state.clone();

    let empty_summary = state
        .open(" ", "Root handoff for child work.")
        .expect_err("empty summary should fail");
    assert_eq!(empty_summary, SpineStateError::EmptySummary);
    assert_eq!(state, before);

    let empty_worklog = state
        .open("root scope", "\n\t")
        .expect_err("empty worklog should fail");
    assert_eq!(empty_worklog, SpineStateError::EmptyWorklog);
    assert_eq!(state, before);
}

#[test]
fn direct_worklog_cannot_be_rewritten() {
    let mut state = SpineState::new();
    state
        .open("root scope", "Root handoff for child work.")
        .expect("open should succeed");
    let before = state.clone();

    let error = state
        .write_direct_worklog(&id(&[1]), "rewrite", "This should fail.")
        .expect_err("rewriting direct worklog should fail");

    assert_eq!(
        error,
        SpineStateError::DirectWorklogAlreadyWritten(id(&[1]))
    );
    assert_eq!(state, before);
}

#[test]
fn visible_spine_excludes_left_sibling_descendants() {
    let mut state = SpineState::new();
    state
        .open("root scope", "Root handoff for child work.")
        .expect("open should succeed");
    state
        .open("first child scope", "Nested handoff.")
        .expect("nested open should succeed");
    state
        .close("nested done", "Nested handoff back to child.")
        .expect("nested close should succeed");
    state
        .next("second child done", "Sibling handoff.")
        .expect("next should succeed");

    assert_eq!(state.cursor(), &id(&[1, 3]));
    assert_eq!(
        state.visible_spine(),
        vec![id(&[1]), id(&[1, 1]), id(&[1, 2]), id(&[1, 3])]
    );
    assert!(state.node(&id(&[1, 1, 1])).is_some());
}
