use super::*;

// Rollback replay over sparse raw history.

#[test]
fn rollback_keeps_open_when_request_item_survives() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw = vec![
        Some(text_item("before")),
        Some(spine_call(SPINE_TOOL_OPEN, "open")),
        None,
    ];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 1).expect("create spine");
    runtime
        .observe_context_item(0, 0, &text_item("before"))
        .expect("observe prefix");
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

    let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw, &[])
        .expect("load spine")
        .expect("sidecar exists");
    let tree = replayed.render_tree().expect("render tree");
    assert!(tree.contains("Cursor: 1.1.1"), "{tree}");
    assert!(tree.contains("- [1.1] Open"), "{tree}");
    assert!(tree.contains("- [1.1.1] Current child task"), "{tree}");
    assert_eq!(
        replayed.materialize_history(&raw).expect("materialize"),
        vec![anchored_text_item(1, "before")]
    );
}

#[test]
fn rollback_skips_open_when_request_item_is_stale() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw = vec![
        Some(text_item("before")),
        None,
        Some(function_output("open")),
    ];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 1).expect("create spine");
    runtime
        .observe_context_item(0, 0, &text_item("before"))
        .expect("observe prefix");
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

    let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw, &[])
        .expect("load spine")
        .expect("sidecar exists");
    let tree = replayed.render_tree().expect("render tree");
    assert!(tree.contains("Cursor: 1.1"), "{tree}");
    assert!(tree.contains("- [1.1] Current"), "{tree}");
    assert_eq!(
        replayed.materialize_history(&raw).expect("materialize"),
        vec![anchored_text_item(1, "before")]
    );
}
