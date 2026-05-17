use super::*;
use crate::spine::ids::NodeId;
use crate::spine::runtime::SpineRuntimeHint;
use crate::spine::state::NodeRecord;
use std::path::Path;

#[test]
fn tree_view_omits_root_and_marks_visible_memories() {
    let mut state = SpineState::new();
    state.next("first child done").expect("finish child");

    assert_eq!(
        render_tree_tool_output(&state, state.cursor()),
        "Current:  1.2\n\n1: live\n    1.1: finished first child done [memory already in context]\n    1.2: Current"
    );
}

#[test]
fn tree_tool_view_can_include_base_path() {
    let mut state = SpineState::new();
    state.next("first child done").expect("finish child");

    assert_eq!(
        render_tree_tool_output_with_base(&state, state.cursor(), Path::new("/tmp/spine")),
        "Current:  1.2\nBase: /tmp/spine\n\n1: live\n    1.1: finished first child done [memory already in context]\n    1.2: Current"
    );
}

#[test]
fn renders_runtime_size_hint_as_standalone_observation_text() {
    let hint = SpineRuntimeHint {
        node_id: NodeId::from_segments(vec![1]),
        estimated_tokens: 63_200,
        threshold_tokens: 60_000,
    };

    assert_eq!(
        render_size_hint(
            &hint,
            Some(&SpineContextBudgetHint {
                used_tokens: 812_300,
                limit_tokens: 900_000,
            }),
        ),
        "\n\nSpine warning: context pressure is high at 812k/900k tokens (88k left); current live node is about 63k. At the next natural boundary, use spine.next/close to move finished work into a memory before Codex auto-compacts the root epoch."
    );
}

#[test]
fn renders_runtime_size_hint_without_budget_as_node_only_text() {
    let hint = SpineRuntimeHint {
        node_id: NodeId::from_segments(vec![1]),
        estimated_tokens: 63_200,
        threshold_tokens: 60_000,
    };

    assert_eq!(
        render_size_hint(&hint, None),
        "\n\nSpine warning: current live node is about 63k tokens and is carried into every request. At a natural boundary, use spine.next/close to move finished work into a memory."
    );
}

#[test]
fn context_budget_pressure_requires_less_than_25_percent_remaining() {
    assert!(!context_budget_is_under_pressure(&SpineContextBudgetHint {
        used_tokens: 750_000,
        limit_tokens: 1_000_000,
    }));
    assert!(context_budget_is_under_pressure(&SpineContextBudgetHint {
        used_tokens: 750_001,
        limit_tokens: 1_000_000,
    }));
}

#[test]
fn tree_view_shows_paths_for_hidden_finished_descendants() {
    let mut state = SpineState::new();
    state.open().expect("open child scope");
    state.next("first leaf done").expect("finish first leaf");
    state.close("child scope").expect("close child scope");

    assert_eq!(
        render_tree_tool_output(&state, state.cursor()),
        "Current:  1.2\n\n1: live\n    1.1: closed child scope [memory already in context]\n        1.1.1: finished first leaf done nodes/1/1/1/memory.md\n        1.1.2: finished nodes/1/1/2/memory.md\n    1.2: Current"
    );
}

#[test]
fn tree_view_resets_after_root_epoch_reset() {
    let mut state = SpineState::new();
    state
        .reset_root_epoch("Context compacted", 7)
        .expect("reset root epoch");

    assert_eq!(
        render_tree_tool_output(&state, state.cursor()),
        "Current:  2.1\n\n1: closed Context compacted [memory already in context]\n    1.1: finished\n2: live\n    2.1: Current"
    );
}

#[test]
fn tree_view_shows_sealed_root_archive_descendants_without_memory_paths() {
    let mut state = SpineState::new();
    state.open().expect("open child");
    state
        .reset_root_epoch("Context compacted", 7)
        .expect("reset root epoch");

    assert_eq!(
        render_tree_tool_output(&state, state.cursor()),
        "Current:  2.1\n\n1: closed Context compacted [memory already in context]\n    1.1: closed\n        1.1.1: finished\n2: live\n    2.1: Current"
    );
}

#[test]
fn tree_view_marks_previous_root_epoch_memory_as_context() {
    let mut state = SpineState::new();
    state
        .reset_root_epoch("first compact", 7)
        .expect("first reset root epoch");
    state
        .reset_root_epoch("second compact", 14)
        .expect("second reset root epoch");

    assert_eq!(
        render_tree_tool_output(&state, state.cursor()),
        "Current:  3.1\n\n1: closed first compact nodes/1/memory.md\n    1.1: finished\n2: closed second compact [memory already in context]\n    2.1: finished\n3: live\n    3.1: Current"
    );
}

#[test]
fn tree_view_marks_legacy_unfinished_nodes_under_closed_ancestor() {
    let state = SpineState::from_records(
        NodeId::from_segments(vec![2, 1]),
        vec![
            NodeRecord {
                node_id: NodeId::from_segments(vec![1]),
                parent_id: None,
                raw_start_ordinal: Some(0),
                status: NodeStatus::Closed,
                summary: Some("Context compacted".to_string()),
            },
            NodeRecord {
                node_id: NodeId::from_segments(vec![1, 1]),
                parent_id: Some(NodeId::from_segments(vec![1])),
                raw_start_ordinal: Some(0),
                status: NodeStatus::Opened,
                summary: None,
            },
            NodeRecord {
                node_id: NodeId::from_segments(vec![1, 1, 1]),
                parent_id: Some(NodeId::from_segments(vec![1, 1])),
                raw_start_ordinal: Some(0),
                status: NodeStatus::Live,
                summary: None,
            },
            NodeRecord {
                node_id: NodeId::from_segments(vec![2]),
                parent_id: None,
                raw_start_ordinal: Some(7),
                status: NodeStatus::Opened,
                summary: None,
            },
            NodeRecord {
                node_id: NodeId::from_segments(vec![2, 1]),
                parent_id: Some(NodeId::from_segments(vec![2])),
                raw_start_ordinal: Some(7),
                status: NodeStatus::Live,
                summary: None,
            },
        ],
    )
    .expect("construct legacy impossible state");

    assert_eq!(
        render_tree_tool_output(&state, state.cursor()),
        "Current:  2.1\n\n1: closed Context compacted [memory already in context]\n    1.1: [undone as compact]\n        1.1.1: [undone as compact]\n2: live\n    2.1: Current"
    );
}
