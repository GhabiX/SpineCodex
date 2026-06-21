use super::*;

#[test]
fn prepared_commit_side_effect_failure_leaves_parse_stack_unadvanced() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    runtime.set_trim_enabled(true);

    open_task_with_token_baselines(
        &mut runtime,
        &mut raw,
        "open-poc-install-fail",
        "poc install fail",
        SpineTokenBaselines {
            provider_input_tokens: Some(10_000),
        },
    );
    append_msg(&mut runtime, &mut raw, "inside");
    observe_spine_request(
        &mut runtime,
        &mut raw,
        SPINE_TOOL_CLOSE,
        "close-poc-install-fail",
    );
    runtime
        .stage_close(
            "close-poc-install-fail".to_string(),
            "test node memory".to_string(),
        )
        .expect("stage close");
    let memory_assembly =
        close_memory_assembly_from_source_plan(&runtime, &raw, "close-poc-install-fail", "1.1.1");
    let close_output = function_output_text(
        "close-poc-install-fail",
        &"large close output for trim candidate ".repeat(40),
    );
    let output_ordinal = u64::try_from(raw.len()).expect("raw ordinal fits");
    let output_context_index = current_context_len(&runtime, &raw)
        .checked_add(1)
        .expect("output context index fits");
    raw.push(Some(close_output));
    runtime.observe_raw_items(1).expect("record output raw");
    runtime
        .observe_context_item(
            output_ordinal,
            output_context_index,
            raw.last()
                .and_then(Option::as_ref)
                .expect("close output item"),
        )
        .expect("observe close output");

    let completed_toolcall = completed_toolcall(
        "close-poc-install-fail",
        vec![
            tool_req(output_ordinal - 1, output_context_index - 1),
            tool_resp(output_ordinal, output_context_index),
        ],
    );
    let prepared = runtime
        .prepare_commit_output_with_toolcall_and_raw_items(
            "close-poc-install-fail",
            Some(memory_assembly),
            SpineTokenBaselines {
                provider_input_tokens: Some(17_500),
            },
            completed_toolcall,
            &raw,
        )
        .expect("prepare close")
        .expect("prepared close");
    let parse_stack_before_install = runtime.parse_stack().clone();
    let tree_before_install = runtime.render_tree().expect("tree before install");
    assert!(
        tree_before_install.contains("[1.1.1] Current poc install fail"),
        "{tree_before_install}"
    );

    let trim_path = runtime.store.trim_path_for_test();
    let parked_trim_path = dir.path().join("parked-trim-before-install.jsonl");
    std::fs::rename(&trim_path, &parked_trim_path).expect("park trim ledger");
    std::fs::create_dir_all(&trim_path).expect("block trim append with directory");

    let err = runtime
        .persist_prepared_commit_side_effects(&prepared)
        .expect_err("trim append failure should fail before install");
    assert!(
        err.to_string().contains("Is a directory")
            || err.to_string().contains("is a directory")
            || err.to_string().contains("directory"),
        "unexpected install error: {err}"
    );
    assert_eq!(
        runtime.parse_stack(),
        &parse_stack_before_install,
        "failed prepared side effects must not advance the parse stack"
    );
    let tree_after_failed_install = runtime
        .render_tree()
        .expect("tree after failed install still renders");
    assert!(
        tree_after_failed_install.contains("[1.1.1] Current poc install fail"),
        "{tree_after_failed_install}"
    );
}

#[test]
fn prepare_close_commit_does_not_install_final_parse_stack() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    open_task(
        &mut runtime,
        &mut raw,
        "open-staged-close",
        "staged close child",
    );
    append_msg(&mut runtime, &mut raw, "child work before staged close");
    let (request, request_raw, request_context) =
        observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_CLOSE, "staged-close");
    runtime
        .stage_close(
            "staged-close".to_string(),
            "staged close memory".to_string(),
        )
        .expect("stage close");
    let memory_assembly =
        close_memory_assembly_from_source_plan(&runtime, &raw, "staged-close", "1.1.1");
    let (_output, output_raw, output_context) =
        observe_function_output(&mut runtime, &mut raw, "staged-close");
    let before_tree = runtime
        .render_tree()
        .expect("render before prepared commit");

    let prepared = runtime
        .prepare_commit_output_with_toolcall_and_raw_items(
            "staged-close",
            Some(memory_assembly),
            SpineTokenBaselines::default(),
            completed_toolcall(
                "staged-close",
                vec![
                    tool_segment(ToolCallSegmentKind::Request, request_raw, request_context),
                    tool_segment(ToolCallSegmentKind::Response, output_raw, output_context),
                ],
            ),
            &raw,
        )
        .expect("prepare close commit")
        .expect("prepared close commit");
    assert!(matches!(prepared.kind(), SpineCommitKind::Close { .. }));
    let publication_plan = prepared
        .publication_plan()
        .expect("close commit should carry publication plan");
    assert_eq!(publication_plan.operation(), "spine.close");
    assert_eq!(publication_plan.suffix_start(), 0);
    assert_eq!(publication_plan.replacement_prefix().len(), 1);
    assert_eq!(
        publication_plan.preserve_host_history_from(),
        request_context
    );
    assert!(
        publication_plan.append_current_tool_response_if_missing(),
        "close publication should append current output when host has not recorded it"
    );
    assert_eq!(
        runtime.render_tree().expect("render after prepared commit"),
        before_tree,
        "prepared close commit must not install the reduced ParseStack before host publication"
    );
    let before_snapshot = runtime
        .build_tree_snapshot()
        .expect("snapshot before installing prepared commit");
    let before_nodes = snapshot_nodes_by_id(&before_snapshot);
    assert_ne!(
        before_nodes["1.1.1"].status,
        SpineTreeNodeStatus::Closed,
        "live tree must not expose closed-node publication before install"
    );

    runtime
        .persist_prepared_commit_side_effects(&prepared)
        .expect("persist prepared close side effects");
    runtime.install_prepared_commit(prepared);
    let after_snapshot = runtime
        .build_tree_snapshot()
        .expect("snapshot after installing prepared commit");
    let after_nodes = snapshot_nodes_by_id(&after_snapshot);
    assert_eq!(
        after_nodes["1.1.1"].status,
        SpineTreeNodeStatus::Closed,
        "installing prepared close commit should advance the live ParseStack"
    );
    assert_eq!(request, spine_call(SPINE_TOOL_CLOSE, "staged-close"));
}
