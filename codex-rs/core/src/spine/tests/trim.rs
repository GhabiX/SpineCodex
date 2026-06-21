use super::*;

#[test]
fn completed_toolcall_tags_long_text_tool_response_for_next_turn_trim() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "long-tool");
    let long_text = "x".repeat(600);
    let output = function_output_text("long-tool", &long_text);
    let raw = vec![Some(request.clone()), Some(output.clone())];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(2).expect("record raw");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe request");
    runtime
        .observe_context_item(1, 1, &output)
        .expect("observe output");
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("long-tool", vec![tool_req(0, 0), tool_resp(1, 1)]),
            &raw,
        )
        .expect("observe completed toolcall");

    let rendered = runtime.materialize_history(&raw).expect("materialize");
    assert_eq!(rendered[0], request);
    assert!(
        function_output_text_content(&rendered[1]).starts_with("[TRIM_ID: trim_0]\n"),
        "rendered long output should expose trim id: {:?}",
        rendered[1]
    );
    assert!(
        function_output_text_content(&rendered[1]).contains(&long_text),
        "tagging must keep original visible output until trim"
    );
    assert_eq!(function_output_text_content(&output), long_text);
    let trim_events = runtime.store.trim_events().expect("trim events");
    assert!(matches!(
        trim_events.as_slice(),
        [LoggedTrimEvent {
            trim_seq: 0,
            event: TrimEvent::Candidate {
                trim_id,
                toolcall_seq: 2,
                raw_ordinal: 1,
                context_index: 1,
                call_id,
                response_kind: TrimResponseKind::FunctionCallOutput,
                ..
            }
        }] if trim_id == "trim_0" && call_id == "long-tool"
    ));
}

#[test]
fn trim_only_runtime_tags_and_trims_without_tree_ledger() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let output = function_output_text("long-tool", &"trim-only output ".repeat(50));
    let raw = vec![Some(output.clone())];
    let mut runtime =
        SpineRuntime::load_or_create_with_jit(&rollout, 0, false).expect("create trim runtime");

    assert!(
        !runtime.store.tree_path_for_test().exists(),
        "trim-only must not create the JIT parser tree ledger"
    );
    runtime.observe_raw_items(1).expect("record raw");
    runtime
        .observe_completed_toolcall_with_raw_items(
            CompletedToolCall {
                call_id: "long-tool".to_string(),
                request_call_ids: vec!["long-tool".to_string()],
                segments: vec![CompletedToolCallSegment {
                    kind: ToolCallSegmentKind::Response,
                    raw_ordinal: 0,
                    context_index: 0,
                }],
            },
            &raw,
        )
        .expect("observe trim-only completed toolcall");

    let projected = runtime
        .project_raw_history_with_trim(&[output.clone()])
        .expect("project trim-only history");
    assert!(
        function_output_text_content(&projected[0]).starts_with("[TRIM_ID: trim_1]\n"),
        "trim-only projection should expose the generated trim id"
    );
    assert_eq!(
        runtime.trim_tool_response("trim_1").expect("trim succeeds"),
        SpineTrimOutcome::Cleared {
            trim_id: "trim_1".to_string()
        }
    );
    let cleared = runtime
        .project_raw_history_with_trim(&[output])
        .expect("project cleared trim-only history");
    assert_eq!(
        function_output_text_content(&cleared[0]),
        crate::spine::model::TOOL_RESULT_CLEARED_MESSAGE
    );
    assert!(
        !runtime.store.tree_path_for_test().exists(),
        "trim-only trim must still not create the JIT parser tree ledger"
    );
}

#[test]
fn trim_only_fork_clone_copies_trim_ledger_without_tree_ledger() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source_rollout = dir.path().join("source.jsonl");
    let target_rollout = dir.path().join("target.jsonl");
    let output = function_output_text("long-tool", &"trim-only fork output ".repeat(50));
    let raw = vec![Some(output.clone())];
    let mut source = SpineRuntime::load_or_create_with_jit(&source_rollout, 0, false)
        .expect("create trim runtime");

    source.observe_raw_items(1).expect("record raw");
    source
        .observe_completed_toolcall_with_raw_items(
            CompletedToolCall {
                call_id: "long-tool".to_string(),
                request_call_ids: vec!["long-tool".to_string()],
                segments: vec![CompletedToolCallSegment {
                    kind: ToolCallSegmentKind::Response,
                    raw_ordinal: 0,
                    context_index: 0,
                }],
            },
            &raw,
        )
        .expect("observe trim-only completed toolcall");
    source.trim_tool_response("trim_1").expect("clear trim id");
    assert!(
        !source.store.tree_path_for_test().exists(),
        "trim-only source must not create the JIT parser tree ledger"
    );

    clone_for_rollout_with_raw_live(&source_rollout, &target_rollout, &[true]);
    let target =
        SpineRuntime::load_or_create_with_jit(&target_rollout, 1, false).expect("load target");
    let projected = target
        .project_raw_history_with_trim(&[output])
        .expect("project target");
    assert_eq!(
        function_output_text_content(&projected[0]),
        crate::spine::model::TOOL_RESULT_CLEARED_MESSAGE
    );
    assert!(
        !target.store.tree_path_for_test().exists(),
        "trim-only clone must not create the JIT parser tree ledger"
    );
}
