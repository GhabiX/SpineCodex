use super::*;

#[test]
fn tree_renders_from_parse_stack_without_mutating_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 1).expect("create spine");

    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(1, 1, &spine_call(SPINE_TOOL_OPEN, "open"))
        .expect("observe open request");
    runtime
        .stage_open("open".to_string(), "child task".to_string())
        .expect("stage open");
    runtime.observe_raw_items(1).expect("record open output");
    runtime
        .observe_context_item(2, 2, &function_output("open"))
        .expect("observe open output");
    runtime
        .maybe_commit_output("open", None)
        .expect("commit open");
    runtime.observe_raw_items(1).expect("observe child item");
    runtime
        .observe_context_item(3, 3, &text_item("inside"))
        .expect("observe child item");
    runtime.observe_raw_items(1).expect("record close request");
    runtime
        .observe_context_item(4, 4, &spine_call(SPINE_TOOL_CLOSE, "close"))
        .expect("observe close request");
    runtime
        .stage_close("close".to_string(), "test node memory".to_string())
        .expect("stage close");
    let suffix_start = match runtime.pending_commit("close").expect("pending close") {
        Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
        other => panic!("expected pending close, got {other:?}"),
    };
    runtime.observe_raw_items(1).expect("record close output");
    runtime
        .observe_context_item(5, 5, &function_output("close"))
        .expect("observe close output");
    runtime
        .maybe_commit_output(
            "close",
            Some(memory_assembly_with_ranges("1.1.1", suffix_start..4, 1..4)),
        )
        .expect("commit close");

    let replayed = SpineRuntime::load_for_rollout(&rollout, runtime.raw_len)
        .expect("load spine")
        .expect("sidecar exists");
    let before = replayed.parse_stack().clone();
    let tree = replayed.render_tree().expect("render tree");
    assert_eq!(replayed.parse_stack(), &before);
    assert_eq!(
        tree,
        replayed.parse_stack().render_tree().expect("render ps")
    );
    assert!(tree.contains("Cursor: 1.1"), "{tree}");
    assert!(tree.contains("Spine Task Tree:"), "{tree}");
    assert!(tree.contains("[1.1] Current"), "{tree}");
    assert!(tree.contains("[1.1.1] Done child task"), "{tree}");
    assert!(
        tree.contains("memory=nodes/1/1/1/Memory.md")
            && tree.contains("trajs=nodes/1/1/1/Trajs.md"),
        "{tree}"
    );
}
