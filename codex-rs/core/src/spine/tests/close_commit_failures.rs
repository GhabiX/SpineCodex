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

#[test]
fn close_failure_does_not_mutate_parse_stack() {
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
        .observe_context_item(3, 3, &spine_call(SPINE_TOOL_CLOSE, "close"))
        .expect("observe close request");
    runtime
        .stage_close("close".to_string(), "test node memory".to_string())
        .expect("stage close");
    runtime.observe_raw_items(1).expect("record close output");
    runtime
        .observe_context_item(4, 4, &function_output("close"))
        .expect("observe close output");

    let parse_stack_before = runtime.parse_stack().clone();
    let tree_before = runtime.render_tree().expect("render tree before failure");
    let events_before = event_log_debug(&runtime);
    let mem_count_before = runtime
        .store
        .mems()
        .expect("read mems before failure")
        .len();
    let err = runtime
        .maybe_commit_output("close", None)
        .expect_err("close without compact output must fail");
    assert!(
        err.to_string()
            .contains("spine.close requires a validated source plan for memory assembly"),
        "unexpected close failure: {err}"
    );

    assert_parse_stack_tree_and_events_unchanged(
        &runtime,
        &parse_stack_before,
        &tree_before,
        &events_before,
    );
    assert_eq!(
        runtime.store.mems().expect("read mems after failure").len(),
        mem_count_before
    );
    assert!(
        runtime
            .pending_commit("close")
            .expect("pending close")
            .is_some()
    );
}

#[test]
fn close_artifact_write_failure_does_not_publish_parse_stack_or_ledger() {
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
        .observe_context_item(3, 3, &spine_call(SPINE_TOOL_CLOSE, "close"))
        .expect("observe close request");
    runtime
        .stage_close("close".to_string(), "test node memory".to_string())
        .expect("stage close");
    runtime.observe_raw_items(1).expect("record close output");
    runtime
        .observe_context_item(4, 4, &function_output("close"))
        .expect("observe close output");

    let parse_stack_before = runtime.parse_stack().clone();
    let tree_before = runtime.render_tree().expect("render tree before failure");
    let events_before = event_log_debug(&runtime);
    let mem_count_before = runtime
        .store
        .mems()
        .expect("read mems before failure")
        .len();
    let body_path = runtime.store.root.join("memory/mem-1-1-1-2-5.md");
    if let Some(parent) = body_path.parent() {
        std::fs::create_dir_all(parent).expect("create memory dir");
    }
    std::fs::create_dir_all(&body_path).expect("block memory body write with directory");

    let err = runtime
        .maybe_commit_output(
            "close",
            Some(memory_assembly_with_context_range("1.1.1", 2..5)),
        )
        .expect_err("artifact write failure should fail commit");
    assert!(
        !err.to_string().is_empty(),
        "expected artifact write failure to surface"
    );
    assert_parse_stack_tree_and_events_unchanged(
        &runtime,
        &parse_stack_before,
        &tree_before,
        &events_before,
    );
    assert_eq!(
        runtime.store.mems().expect("read mems after failure").len(),
        mem_count_before
    );
    assert!(
        !runtime.store.root.join("nodes/1/1/1/Memory.md").exists(),
        "artifact failure must not flush node Memory.md"
    );
    assert!(
        matches!(
            runtime.pending_commit("close").expect("pending retained"),
            Some(SpinePendingCommit::Close { .. })
        ),
        "failed artifact commit should retain pending close"
    );
}

#[test]
fn close_persistence_failure_leaves_retryable_close_token() {
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
        .observe_context_item(3, 3, &spine_call(SPINE_TOOL_CLOSE, "close"))
        .expect("observe close request");
    runtime
        .stage_close("close".to_string(), "test node memory".to_string())
        .expect("stage close");
    let suffix_start = match runtime.pending_commit("close").expect("pending close") {
        Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
        other => panic!("expected pending close, got {other:?}"),
    };
    let close_request_index = 3;
    runtime.observe_raw_items(1).expect("record close output");
    runtime
        .observe_context_item(4, 4, &function_output("close"))
        .expect("observe close output");

    let tree_before = runtime.render_tree().expect("render tree before failure");
    let events_before = event_log_debug(&runtime);
    std::fs::create_dir(runtime.store.mem_path()).expect("poison mem ledger path");

    let err = runtime
        .maybe_commit_output(
            "close",
            Some(memory_assembly_with_context_range(
                "1.1.1",
                suffix_start..close_request_index,
            )),
        )
        .expect_err("close mem persistence failure must fail");
    assert!(
        err.to_string().contains("Is a directory")
            || err.to_string().contains("os error 21")
            || err.to_string().contains("Permission denied"),
        "unexpected close persistence failure: {err}"
    );

    assert_eq!(
        runtime.render_tree().expect("render tree after failure"),
        tree_before,
        "failed close must not publish the reduced task tree"
    );
    assert_eq!(
        event_log_debug(&runtime),
        events_before,
        "failed close must not publish ledger events"
    );
    assert!(
        runtime
            .parse_stack()
            .symbols
            .iter()
            .any(|symbol| matches!(symbol, Symbol::Control(ControlSymbol::Close(_)))),
        "failed close must retain the zero-width Close token for retry"
    );
    assert!(
        runtime
            .pending_commit("close")
            .expect("pending close")
            .is_some()
    );
}
