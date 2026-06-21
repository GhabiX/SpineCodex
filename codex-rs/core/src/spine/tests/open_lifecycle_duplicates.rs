use super::*;

#[test]
fn duplicate_open_call_id_does_not_create_second_child() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    let request = spine_call(SPINE_TOOL_OPEN, "dup-open");
    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe first open request");

    runtime
        .observe_raw_items(1)
        .expect("record duplicate request");
    let err = runtime
        .observe_context_item(1, 1, &request)
        .expect_err("duplicate open request anchor must fail fast");
    assert!(
        err.to_string()
            .contains("duplicate spine.open request anchor for dup-open"),
        "unexpected duplicate error: {err}"
    );

    runtime
        .stage_open("dup-open".to_string(), "only child".to_string())
        .expect("stage open");
    let output = function_output("dup-open");
    runtime.observe_raw_items(1).expect("record open output");
    runtime
        .observe_context_item(2, 2, &output)
        .expect("observe open output");
    runtime
        .maybe_commit_output("dup-open", None)
        .expect("commit open");
    let events_after_first_commit = event_log(&runtime);
    let event_debug_after_first_commit = event_log_debug(&runtime);
    assert_eq!(
        events_after_first_commit
            .iter()
            .filter(
                |event| matches!(event, SpineLedgerEvent::Open { summary, .. } if summary == "only child")
            )
            .count(),
        1
    );
    assert_eq!(
        runtime
            .maybe_commit_output("dup-open", None)
            .expect("duplicate output commit should be no-op"),
        None
    );
    assert_eq!(event_log_debug(&runtime), event_debug_after_first_commit);
    let tree = runtime.render_tree().expect("render tree");
    assert!(tree.contains("Cursor: 1.1.1"), "{tree}");
    assert!(tree.contains("Spine Task Tree:"), "{tree}");
    assert!(tree.contains("- [1.1] Open"), "{tree}");
    assert!(tree.contains("- [1.1.1] Current only child"), "{tree}");
}
