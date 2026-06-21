use super::*;

#[test]
fn m0_trace_golden_baseline_projects_tokens_to_hps() {
    let dir = tempfile::tempdir().expect("tempdir");

    {
        let rollout = dir.path().join("message-tool.jsonl");
        let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
        let mut raw = Vec::new();
        append_msg(&mut runtime, &mut raw, "m0 user message");

        let request_context = current_context_len(&runtime, &raw);
        let (request, request_raw, _) = observe_item_at_context_index(
            &mut runtime,
            &mut raw,
            ordinary_call("shell_command", "m0-tool"),
            request_context,
        );
        let (output, output_raw, output_context) = observe_item_at_context_index(
            &mut runtime,
            &mut raw,
            function_output_text("m0-tool", "pwd ok"),
            request_context + 1,
        );
        runtime
            .observe_completed_toolcall(completed_toolcall(
                "m0-tool",
                vec![
                    tool_req(request_raw, request_context),
                    tool_resp(output_raw, output_context),
                ],
            ))
            .expect("observe ordinary toolcall");

        assert_eq!(
            materialized_trace_signature(&runtime, &raw),
            vec![
                "user:m0 user message".to_string(),
                response_item_trace_signature(&request),
                response_item_trace_signature(&output),
            ],
            "ordinary msg/tool trace must project as raw msg plus one completed toolcall leaf"
        );
    }

    {
        let rollout = dir.path().join("open.jsonl");
        let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
        let mut raw = Vec::new();
        open_task(&mut runtime, &mut raw, "m0-open", "m0 child");

        assert_eq!(
            materialized_trace_signature(&runtime, &raw),
            vec![
                "spine-call:open:m0-open".to_string(),
                "tool-output:m0-open:ok".to_string(),
            ],
            "open emits open toolcall and makes that complete toolcall the child leaf"
        );
    }

    {
        let rollout = dir.path().join("close.jsonl");
        let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
        let mut raw = Vec::new();
        append_msg(&mut runtime, &mut raw, "m0 close body");
        close_task(&mut runtime, &mut raw, "m0-close", "1.1");

        assert_eq!(
            materialized_trace_signature(&runtime, &raw),
            vec![
                "memory:# Spine Memory 1.1".to_string(),
                "spine-call:close:m0-close".to_string(),
                "tool-output:m0-close:ok".to_string(),
            ],
            "close emits close toolcall, replaces live suffix with one memory, and keeps the carrier toolcall in the parent"
        );
    }

    {
        let rollout = dir.path().join("next.jsonl");
        let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
        let mut raw = Vec::new();
        append_msg(&mut runtime, &mut raw, "m0 next body");
        next_task(&mut runtime, &mut raw, "m0-next", "1.1", "m0 sibling");

        assert_eq!(
            materialized_trace_signature(&runtime, &raw),
            vec![
                "memory:# Spine Memory 1.1".to_string(),
                "spine-call:next:m0-next".to_string(),
                "tool-output:m0-next:ok".to_string(),
            ],
            "next emits close open toolcall, replaces the closed suffix with memory, and makes the carrier toolcall the sibling's first leaf"
        );
    }

    {
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
                    request_call_ids: vec![
                        "m0-group-close".to_string(),
                        "m0-group-tool".to_string(),
                    ],
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
}
