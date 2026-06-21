use super::*;

#[test]
fn abort_pending_clears_receipt_before_it_becomes_pending_transition() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    open_task(&mut runtime, &mut raw, "open", "child task");
    append_msg(&mut runtime, &mut raw, "work inside child");
    observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_CLOSE, "close");
    runtime
        .record_close_tool_receipt("close".to_string(), "test node memory".to_string())
        .expect("record close receipt");

    assert!(runtime.has_close_like_control_receipt("close"));
    assert!(runtime.abort_pending("close"));
    assert!(!runtime.has_close_like_control_receipt("close"));
    assert!(!runtime.control_call_ids.contains("close"));
    assert!(
        runtime
            .pending_commit("close")
            .expect("cleared receipt")
            .is_none()
    );
}
