use super::*;

#[test]
fn completed_toolcall_rejects_request_after_response() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request_1 = ordinary_call("shell_command", "tool-1");
    let output_1 = function_output("tool-1");
    let request_2 = ordinary_call("tool_search", "tool-2");
    let output_2 = function_output("tool-2");
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime
        .observe_raw_items(4)
        .expect("record interleaved raw");
    runtime
        .observe_context_item(0, 0, &request_1)
        .expect("observe first request");
    runtime
        .observe_context_item(1, 1, &output_1)
        .expect("observe first response");
    runtime
        .observe_context_item(2, 2, &request_2)
        .expect("observe second request");
    runtime
        .observe_context_item(3, 3, &output_2)
        .expect("observe second response");

    let err = runtime
        .observe_completed_toolcall(CompletedToolCall {
            call_id: "tool-1".to_string(),
            request_call_ids: vec!["tool-1".to_string(), "tool-2".to_string()],
            segments: vec![
                CompletedToolCallSegment {
                    kind: ToolCallSegmentKind::Request,
                    raw_ordinal: 0,
                    context_index: 0,
                },
                CompletedToolCallSegment {
                    kind: ToolCallSegmentKind::Response,
                    raw_ordinal: 1,
                    context_index: 1,
                },
                CompletedToolCallSegment {
                    kind: ToolCallSegmentKind::Request,
                    raw_ordinal: 2,
                    context_index: 2,
                },
                CompletedToolCallSegment {
                    kind: ToolCallSegmentKind::Response,
                    raw_ordinal: 3,
                    context_index: 3,
                },
            ],
        })
        .expect_err("toolcall must have all requests before responses");
    assert!(
        err.to_string().contains("appears after a response segment"),
        "unexpected completed toolcall error: {err}"
    );
}
