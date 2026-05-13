use super::*;
use crate::spine::ids::NodeId;
use crate::spine::runtime::SpineRuntimeHint;
use std::path::Path;

#[test]
fn tree_view_omits_root_and_marks_visible_worklogs() {
    let mut state = SpineState::new();
    state.next("first child done").expect("finish child");

    assert_eq!(
        render_tree_tool_output(&state, state.cursor()),
        "Current:  1.2\n\n1.1: finished first child done [worklog already in context]\n1.2: Current"
    );
}

#[test]
fn tree_tool_view_can_include_base_path() {
    let mut state = SpineState::new();
    state.next("first child done").expect("finish child");

    assert_eq!(
        render_tree_tool_output_with_base(&state, state.cursor(), Path::new("/tmp/spine")),
        "Current:  1.2\nBase: /tmp/spine\n\n1.1: finished first child done [worklog already in context]\n1.2: Current"
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
        "\n\nSpine hint: context is about 812k/900k tokens (88k left); current live node is about 63k. At a natural boundary, use spine.next/close to move finished work into a worklog before Codex auto-compacts the root epoch."
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
        "\n\nSpine hint: current live node is about 63k tokens and is carried into every request. At a natural boundary, use spine.next/close to move finished work into a worklog."
    );
}

#[test]
fn tree_view_shows_paths_for_hidden_finished_descendants() {
    let mut state = SpineState::new();
    state.open().expect("open child scope");
    state.next("first leaf done").expect("finish first leaf");
    state.close("child scope").expect("close child scope");

    assert_eq!(
        render_tree_tool_output(&state, state.cursor()),
        "Current:  1.2\n\n1.1: closed child scope [worklog already in context]\n    1.1.1: finished first leaf done nodes/1/1/1/worklog.md\n    1.1.2: finished nodes/1/1/2/worklog.md\n1.2: Current"
    );
}

#[test]
fn tree_view_resets_after_root_epoch_reset() {
    let mut state = SpineState::new();
    state.open().expect("open child");
    state.reset_root_epoch(7).expect("reset root epoch");

    assert_eq!(
        render_tree_tool_output(&state, state.cursor()),
        "Current:  1.1\n\n1.1: Current"
    );
}
