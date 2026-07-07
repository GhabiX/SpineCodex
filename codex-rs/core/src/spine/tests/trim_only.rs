use super::*;

#[test]
fn trim_only_runtime_tags_and_trims_without_tree_ledger() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let output = function_output_text("long-tool", &trim_candidate_text("trim-only output "));
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
