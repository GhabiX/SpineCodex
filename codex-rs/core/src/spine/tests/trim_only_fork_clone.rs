use super::*;

#[test]
fn trim_only_fork_clone_copies_trim_ledger_without_tree_ledger() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source_rollout = dir.path().join("source.jsonl");
    let target_rollout = dir.path().join("target.jsonl");
    let output = function_output_text("long-tool", &trim_candidate_text("trim-only fork output "));
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
