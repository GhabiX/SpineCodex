use super::*;

#[test]
fn closed_memory_context_accounting_rejects_negative_memory_delta() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw = vec![
        Some(text_item("before")),
        Some(spine_call(SPINE_TOOL_OPEN, "open")),
        Some(function_output("open")),
        Some(text_item("inside")),
        Some(spine_call(SPINE_TOOL_CLOSE, "close")),
        Some(function_output("close")),
    ];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(1).expect("record before");
    runtime
        .observe_context_item(0, 0, &text_item("before"))
        .expect("observe before");
    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(1, 1, &spine_call(SPINE_TOOL_OPEN, "open"))
        .expect("observe open request");
    runtime
        .stage_open("open".to_string(), "accounted child".to_string())
        .expect("stage open");
    runtime.observe_raw_items(1).expect("record open output");
    runtime
        .observe_context_item(2, 2, &function_output("open"))
        .expect("observe open output");
    runtime
        .maybe_commit_output_with_open_input_tokens("open", None, Some(10_000))
        .expect("commit open");
    runtime.observe_raw_items(1).expect("record child item");
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
        .maybe_commit_output_with_open_input_tokens(
            "close",
            Some(memory_assembly_with_ranges("1.1.1", suffix_start..4, 1..4)),
            Some(17_500),
        )
        .expect("commit close");

    let captured = runtime
        .capture_closed_memory_context_accounting(9_999)
        .expect("negative memory delta should not corrupt accounting");
    assert!(!captured);
    assert!(
        runtime
            .store
            .mem_accounting()
            .expect("memory accounting")
            .is_empty()
    );

    let materialized = runtime
        .materialize_variable_context_for_test(&raw)
        .expect("materialize");
    assert_eq!(materialized.len(), 4);
}
