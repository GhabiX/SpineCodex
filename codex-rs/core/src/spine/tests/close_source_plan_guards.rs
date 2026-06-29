use super::*;

fn developer_fixed_prefix_item(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "developer".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
    }
}

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
        .materialize_variable_context_for_test(&raw)
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

#[test]
fn source_plan_reads_mutable_refs_via_lens() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    open_task(
        &mut runtime,
        &mut raw,
        "open-fixed-prefix-source-plan",
        "fixed prefix source plan child",
    );
    append_msg(&mut runtime, &mut raw, "live item before close");
    let (_request, _request_raw, request_context) = observe_spine_request(
        &mut runtime,
        &mut raw,
        SPINE_TOOL_CLOSE,
        "close-fixed-prefix-source-plan",
    );
    runtime
        .stage_close(
            "close-fixed-prefix-source-plan".to_string(),
            "source plan memory".to_string(),
        )
        .expect("stage close");

    let mut host_history = vec![developer_fixed_prefix_item("fixed developer prefix")];
    host_history.extend(
        runtime
            .materialize_variable_context_for_test(&raw)
            .expect("materialize current h(PS)"),
    );
    let (node, suffix_start) = match runtime
        .pending_commit("close-fixed-prefix-source-plan")
        .expect("pending close should be readable")
    {
        Some(SpinePendingCommit::Close {
            node, suffix_start, ..
        }) => (node, suffix_start),
        other => panic!("expected pending close, got {other:?}"),
    };

    let source_plan = runtime
        .build_close_source_plan(
            &host_history,
            &node,
            suffix_start,
            request_context,
            "close-fixed-prefix-source-plan",
        )
        .expect("source plan must read mutable refs through HostHistoryLens");

    assert_eq!(
        source_plan.source_context_range,
        suffix_start..request_context
    );
    assert!(
        source_plan
            .entries
            .iter()
            .any(|entry| entry.context_index == suffix_start),
        "source plan should keep mutable context indices despite fixed host prefix"
    );
}
