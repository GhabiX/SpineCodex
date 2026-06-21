use super::*;

#[test]
fn duplicate_control_tool_receipt_preserves_original_receipt() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    open_task(&mut runtime, &mut raw, "open", "child task");
    append_msg(&mut runtime, &mut raw, "work inside child");
    observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_CLOSE, "close");
    runtime
        .record_close_tool_receipt("close".to_string(), "first memory".to_string())
        .expect("record first receipt");

    let err = runtime
        .record_close_tool_receipt("close".to_string(), "second memory".to_string())
        .expect_err("duplicate receipt must fail");
    assert!(err.to_string().contains("duplicate Spine control receipt"));
    assert!(matches!(
        runtime.pending_commit("close").expect("receipt pending view"),
        Some(SpinePendingCommit::Close { memory, .. }) if memory == "first memory"
    ));
}
