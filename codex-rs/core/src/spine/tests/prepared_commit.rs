use super::*;

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
    let history_update = prepared
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

    let install = prepared.into_install_for_test();
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
