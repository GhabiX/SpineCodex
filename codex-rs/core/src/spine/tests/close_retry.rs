use super::*;

#[test]
fn duplicate_close_call_id_does_not_create_second_memory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(0, 0, &spine_call(SPINE_TOOL_OPEN, "open"))
        .expect("observe open request");
    runtime
        .stage_open("open".to_string(), "child".to_string())
        .expect("stage open");
    runtime.observe_raw_items(1).expect("record open output");
    runtime
        .observe_context_item(1, 1, &function_output("open"))
        .expect("observe open output");
    runtime
        .maybe_commit_output("open", None)
        .expect("commit open");
    runtime.observe_raw_items(1).expect("record child raw");
    runtime
        .observe_context_item(2, 2, &text_item("inside"))
        .expect("observe child raw");
    runtime.observe_raw_items(1).expect("record close request");
    runtime
        .observe_context_item(3, 3, &spine_call(SPINE_TOOL_CLOSE, "dup-close"))
        .expect("observe close request");
    runtime
        .stage_close("dup-close".to_string(), "test node memory".to_string())
        .expect("stage close");
    let suffix_start = match runtime
        .pending_commit("dup-close")
        .expect("pending close should be readable")
    {
        Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
        other => panic!("expected pending close, got {other:?}"),
    };
    let close_request_index = 3;
    runtime.observe_raw_items(1).expect("record close output");
    runtime
        .observe_context_item(4, 4, &function_output("dup-close"))
        .expect("observe close output");
    runtime
        .maybe_commit_output(
            "dup-close",
            Some(memory_assembly_with_context_range(
                "1.1.1",
                suffix_start..close_request_index,
            )),
        )
        .expect("commit close");

    let events_after_first_commit = event_log_debug(&runtime);
    let mems_after_first_commit = runtime.store.mems().expect("read mems");
    assert_eq!(mems_after_first_commit.len(), 1);
    assert_eq!(
        runtime
            .maybe_commit_output(
                "dup-close",
                Some(memory_assembly_with_context_range("1.1.1", suffix_start..5)),
            )
            .expect("duplicate close output commit should be no-op"),
        None
    );
    assert_eq!(event_log_debug(&runtime), events_after_first_commit);
    assert_eq!(
        runtime
            .store
            .mems()
            .expect("read mems after duplicate")
            .len(),
        1
    );
}
