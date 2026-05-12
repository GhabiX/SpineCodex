use super::*;
use crate::spine::ids::NodeId;
use crate::spine::runtime::SpineRuntimeHint;
use std::path::Path;

#[test]
fn tree_view_omits_root_and_marks_visible_worklogs() {
    let mut state = SpineState::new();
    state.open("parent scope").expect("open parent");
    state.next("first child done").expect("finish child");

    assert_eq!(
        render_tree_tool_output(&state, state.cursor()),
        "Current:  2\n\n1: finished first child done [worklog already in context]\n2: Current"
    );
}

#[test]
fn tree_tool_view_can_include_base_path() {
    let mut state = SpineState::new();
    state.open("parent scope").expect("open parent");
    state.next("first child done").expect("finish child");

    assert_eq!(
        render_tree_tool_output_with_base(&state, state.cursor(), Path::new("/tmp/spine")),
        "Current:  2\nBase: /tmp/spine\n\n1: finished first child done [worklog already in context]\n2: Current"
    );
}

#[test]
fn tree_tool_view_can_include_runtime_size_hint() {
    let state = SpineState::new();
    let hint = SpineRuntimeHint {
        node_id: NodeId::from_segments(vec![1]),
        estimated_tokens: 63_200,
        threshold_tokens: 60_000,
    };

    assert_eq!(
        render_tree_tool_output_with_base_and_hint(
            &state,
            state.cursor(),
            Path::new("/tmp/spine"),
            Some(&hint),
        ),
        "Current:  root\nBase: /tmp/spine\n\n(empty)\n\nSpine hint: current node raw trace is about 63k tokens. If this scope is complete, finish it cleanly and use spine.next or spine.close before starting work that can rely on the worklog."
    );
}

#[test]
fn tree_view_shows_paths_for_hidden_finished_descendants() {
    let mut state = SpineState::new();
    state.open("parent scope").expect("open parent");
    state.open("child scope").expect("open child");
    state.next("first leaf done").expect("finish first leaf");
    state.close("second leaf done").expect("close child scope");

    assert_eq!(
        render_tree_tool_output(&state, state.cursor()),
        "Current:  2\n\n1: closed child scope [worklog already in context]\n    1.1: finished first leaf done nodes/1/1/1/worklog.md\n    1.2: finished second leaf done nodes/1/1/2/worklog.md\n2: Current"
    );
}

#[test]
fn tree_view_marks_unfinished_descendants_after_root_epoch_archive() {
    let mut state = SpineState::new();
    state.open("active scope").expect("open root epoch");
    state.open("unfinished child").expect("open child");
    state
        .archive_current_root_epoch("context compacted")
        .expect("archive root epoch");

    assert_eq!(
        render_tree_tool_output(&state, state.cursor()),
        "Current:  2\n\n1: closed unfinished child [worklog already in context]\n    1.1: [undone as compact]\n2: Current"
    );
}
