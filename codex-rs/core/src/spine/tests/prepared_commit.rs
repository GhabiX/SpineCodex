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
    let output_item = raw[output_raw as usize]
        .as_ref()
        .expect("output item")
        .clone();
    let host_history_before_output = raw
        .iter()
        .take(output_context)
        .filter_map(Clone::clone)
        .collect::<Vec<_>>();
    let mut build_history_update = Some(
        |call_id: &str,
         operation,
         suffix_start,
         expected_history: Vec<ResponseItem>,
         replacement| {
            (
                call_id.to_string(),
                operation,
                suffix_start,
                expected_history,
                replacement,
            )
        },
    );
    let install = prepared.into_install_for_test();
    let history_update = install
        .apply_publication_history_update(
            "staged-close",
            &output_item,
            false,
            &host_history_before_output,
            &mut build_history_update,
        )
        .expect("publication update")
        .expect("close commit should publish host history");
    assert_eq!(history_update.0, "staged-close");
    assert_eq!(history_update.1, "spine.close");
    assert_eq!(history_update.2, 0);
    assert_eq!(
        history_update.4.last(),
        Some(&output_item),
        "close publication should append current output when host has not recorded it"
    );
    assert!(
        history_update.4.len() < history_update.3.len(),
        "close publication should compact the closed suffix"
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
        .persist_prepared_commit_install_side_effects_for_test(&install)
        .expect("persist prepared close side effects");
    runtime.install_prepared_commit_install_for_test(install);
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

#[test]
fn close_publication_fixed_prefix_converts_mutable_toolcall_start_to_full_host() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    open_task(
        &mut runtime,
        &mut raw,
        "open-fixed-prefix-close",
        "fixed prefix close child",
    );
    append_msg(
        &mut runtime,
        &mut raw,
        "child work before fixed prefix close",
    );
    let (_request, request_raw, request_context) = observe_spine_request(
        &mut runtime,
        &mut raw,
        SPINE_TOOL_CLOSE,
        "fixed-prefix-close",
    );
    runtime
        .stage_close(
            "fixed-prefix-close".to_string(),
            "fixed prefix close memory".to_string(),
        )
        .expect("stage close");
    let memory_assembly =
        close_memory_assembly_from_source_plan(&runtime, &raw, "fixed-prefix-close", "1.1.1");
    let (_output, output_raw, output_context) =
        observe_function_output(&mut runtime, &mut raw, "fixed-prefix-close");

    let prepared = runtime
        .prepare_commit_output_with_toolcall_and_raw_items(
            "fixed-prefix-close",
            Some(memory_assembly),
            SpineTokenBaselines::default(),
            completed_toolcall(
                "fixed-prefix-close",
                vec![
                    tool_segment(ToolCallSegmentKind::Request, request_raw, request_context),
                    tool_segment(ToolCallSegmentKind::Response, output_raw, output_context),
                ],
            ),
            &raw,
        )
        .expect("prepare close commit")
        .expect("prepared close commit");
    let output_item = raw[output_raw as usize]
        .as_ref()
        .expect("output item")
        .clone();
    let fixed_prefix = developer_fixed_prefix_item("fixed developer prefix");
    let mut host_history_before_output = vec![fixed_prefix.clone()];
    host_history_before_output.extend(raw.iter().take(output_context).filter_map(Clone::clone));
    let mut build_history_update = Some(
        |call_id: &str,
         operation,
         suffix_start,
         expected_history: Vec<ResponseItem>,
         replacement| {
            (
                call_id.to_string(),
                operation,
                suffix_start,
                expected_history,
                replacement,
            )
        },
    );
    let install = prepared.into_install_for_test();
    let history_update = install
        .apply_publication_history_update(
            "fixed-prefix-close",
            &output_item,
            false,
            &host_history_before_output,
            &mut build_history_update,
        )
        .expect("publication update")
        .expect("close commit should publish host history");

    assert_eq!(history_update.1, "spine.close");
    assert_eq!(
        history_update.2, 1,
        "mutable suffix_start 0 must publish as full host index 1 after the fixed prefix"
    );
    assert_eq!(history_update.3.first(), Some(&fixed_prefix));
    assert_eq!(
        history_update.4.last(),
        Some(&output_item),
        "the completed close tool response must stay paired with its request"
    );
}

#[test]
fn candidate_history_rejects_orphan_tool_outputs_before_publish() {
    let (err, replace_called) = reject_orphan_candidate_history(vec![custom_tool_output_text(
        "custom-output",
        "orphan output",
    )]);

    assert!(
        err.contains("orphan custom tool call output"),
        "unexpected error: {err}"
    );
    assert!(
        !replace_called,
        "orphan candidate history must be rejected before replace_history_suffix"
    );
}

#[test]
fn close_orphan_custom_tool_output_regression() {
    let (err, replace_called) = reject_orphan_candidate_history(vec![custom_tool_output_text(
        "custom-output",
        "orphan output",
    )]);

    assert!(
        err.contains("orphan custom tool call output"),
        "unexpected error: {err}"
    );
    assert!(
        !replace_called,
        "orphan custom output must be rejected before replace_history_suffix"
    );
}

#[test]
fn close_orphan_function_output_regression() {
    let (err, replace_called) = reject_orphan_candidate_history(vec![function_output_text(
        "function-output",
        "orphan output",
    )]);

    assert!(
        err.contains("orphan function call output"),
        "unexpected error: {err}"
    );
    assert!(
        !replace_called,
        "orphan candidate history must be rejected before replace_history_suffix"
    );
}

#[test]
fn append_current_custom_tool_response_requires_matching_request() {
    let (err, replace_called) = reject_orphan_candidate_history(vec![custom_tool_output_text(
        "custom-output",
        "orphan output",
    )]);

    assert!(
        err.contains("orphan custom tool call output"),
        "unexpected error: {err}"
    );
    assert!(
        !replace_called,
        "custom tool output must not be appended without matching request"
    );
}

fn reject_orphan_candidate_history(replacement: Vec<ResponseItem>) -> (String, bool) {
    let current_history = vec![text_item("kept prefix")];
    let update = SpineHistoryUpdate {
        call_id: "orphan-output".to_string(),
        operation: "spine.close",
        suffix_start: 1,
        expected_history: current_history.clone(),
        replacement,
        reference_context_item: None,
    };
    let effect = SpineHostEffect::ReplaceHistory(update);
    let mut replace_called = false;

    let err = match effect.apply_history_update_or_self(
        &current_history,
        |_range, _replacement, _reference| {
            replace_called = true;
            Ok(())
        },
    ) {
        Ok(_) => panic!("custom tool output without request must fail before host publish"),
        Err(err) => err,
    };
    (err, replace_called)
}
