use super::*;

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
