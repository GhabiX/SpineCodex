use super::*;

#[test]
fn m0_trace_grouped_close_keeps_completed_toolcall_leaf() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert_m0_trace_grouped_close_keeps_completed_toolcall_leaf(&dir);
}

fn assert_m0_trace_grouped_close_keeps_completed_toolcall_leaf(dir: &tempfile::TempDir) {
    let rollout = dir.path().join("grouped-close.jsonl");
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();
    append_msg(&mut runtime, &mut raw, "m0 grouped close body");
    let (_close_request, close_request_raw, close_request_context) =
        observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_CLOSE, "m0-group-close");
    runtime
        .stage_close(
            "m0-group-close".to_string(),
            "m0 grouped close memory".to_string(),
        )
        .expect("stage grouped close");
    let (_ordinary_request, ordinary_request_raw, ordinary_request_context) =
        observe_item_at_context_index(
            &mut runtime,
            &mut raw,
            ordinary_call("shell_command", "m0-group-tool"),
            close_request_context + 1,
        );
    let memory_assembly =
        close_memory_assembly_from_source_plan(&runtime, &raw, "m0-group-close", "1.1");
    let (_close_output, close_output_raw, close_output_context) = observe_item_at_context_index(
        &mut runtime,
        &mut raw,
        function_output("m0-group-close"),
        close_request_context + 2,
    );
    let (_ordinary_output, ordinary_output_raw, ordinary_output_context) =
        observe_item_at_context_index(
            &mut runtime,
            &mut raw,
            function_output_text("m0-group-tool", "group tool ok"),
            close_request_context + 3,
        );
    runtime
        .maybe_commit_output_with_toolcall_and_raw_items(
            "m0-group-close",
            Some(memory_assembly),
            SpineTokenBaselines::default(),
            CompletedToolCall {
                call_id: "m0-group-close".to_string(),
                request_call_ids: vec!["m0-group-close".to_string(), "m0-group-tool".to_string()],
                segments: vec![
                    CompletedToolCallSegment {
                        kind: ToolCallSegmentKind::Request,
                        raw_ordinal: close_request_raw,
                        context_index: close_request_context,
                    },
                    CompletedToolCallSegment {
                        kind: ToolCallSegmentKind::Request,
                        raw_ordinal: ordinary_request_raw,
                        context_index: ordinary_request_context,
                    },
                    CompletedToolCallSegment {
                        kind: ToolCallSegmentKind::Response,
                        raw_ordinal: close_output_raw,
                        context_index: close_output_context,
                    },
                    CompletedToolCallSegment {
                        kind: ToolCallSegmentKind::Response,
                        raw_ordinal: ordinary_output_raw,
                        context_index: ordinary_output_context,
                    },
                ],
            },
            &raw,
        )
        .expect("commit grouped close")
        .expect("grouped close should commit");

    assert_eq!(
        materialized_trace_signature(&runtime, &raw),
        vec![
            "memory:# Spine Memory 1.1".to_string(),
            "spine-call:close:m0-group-close".to_string(),
            "tool-call:shell_command:m0-group-tool".to_string(),
            "tool-output:m0-group-close:ok".to_string(),
            "tool-output:m0-group-tool:group tool ok".to_string(),
        ],
        "grouped close must keep the full completed toolcall as one parent leaf after the close reduction"
    );
}
