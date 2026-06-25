use super::*;

#[test]
fn close_source_plan_rejects_host_history_not_matching_hps_projection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    for index in 0..5 {
        append_msg_with_context_index(&mut runtime, &mut raw, &format!("prefix {index}"), index);
    }
    observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_OPEN, "open-dup");
    runtime
        .stage_open(
            "open-dup".to_string(),
            "duplicate provenance task".to_string(),
        )
        .expect("stage open");
    observe_function_output(&mut runtime, &mut raw, "open-dup");
    runtime
        .maybe_commit_output("open-dup", None)
        .expect("commit open");

    append_msg(&mut runtime, &mut raw, "first live item");
    append_msg(&mut runtime, &mut raw, "second live item");
    observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_CLOSE, "close-dup");
    runtime
        .stage_close("close-dup".to_string(), "test node memory".to_string())
        .expect("stage close");

    let mut host_history = runtime
        .materialize_history_for_test(&raw)
        .expect("materialize current h(PS)");
    host_history.insert(8, text_item("host item not represented by h(PS)"));

    let (node, suffix_start) = match runtime
        .pending_commit("close-dup")
        .expect("pending close should be readable")
    {
        Some(SpinePendingCommit::Close {
            node, suffix_start, ..
        }) => (node, suffix_start),
        other => panic!("expected pending close, got {other:?}"),
    };
    let toolcall_start = host_history
        .iter()
        .position(|item| matches!(item, ResponseItem::FunctionCall { call_id: existing, .. } if existing == "close-dup"))
        .unwrap_or(host_history.len());
    let err = runtime
        .build_close_source_plan(
            &host_history,
            &node,
            suffix_start,
            toolcall_start,
            "close-dup",
        )
        .expect_err("host/projection mismatch must fail");
    assert!(
        err.to_string().contains("h(PS) suffix projects"),
        "unexpected host/projection mismatch error: {err}"
    );
}
