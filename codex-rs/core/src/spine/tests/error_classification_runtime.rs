use super::*;

#[test]
fn spine_error_classifies_tool_use_operation_and_compact_failures() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    let err = runtime
        .stage_open("empty-open".to_string(), "   ".to_string())
        .expect_err("empty open summary must fail");
    assert_eq!(err.class(), SpineErrorClass::ToolUse);
    assert!(
        err.to_string()
            .contains("spine.open summary must not be empty")
    );

    let err = runtime
        .stage_open("missing-anchor".to_string(), "child".to_string())
        .expect_err("open without observed request anchor must fail");
    assert_eq!(err.class(), SpineErrorClass::Operation);
    assert!(
        err.to_string()
            .contains("missing spine.open request anchor")
    );

    let mut raw = Vec::new();
    runtime.observe_raw_items(1).expect("record user raw item");
    raw.push(Some(text_item("live suffix")));
    runtime
        .observe_context_item(0, 0, &text_item("live suffix"))
        .expect("observe user msg");
    observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_CLOSE, "close");
    runtime
        .stage_close("close".to_string(), "test node memory".to_string())
        .expect("stage close");
    let err = runtime
        .maybe_commit_output("close", None)
        .expect_err("close without compact result must fail");
    assert_eq!(err.class(), SpineErrorClass::CompactFailure);
    assert!(
        err.to_string()
            .contains("spine.close requires a validated source plan for memory assembly")
    );
    assert!(!err.should_invalidate_runtime());
}
