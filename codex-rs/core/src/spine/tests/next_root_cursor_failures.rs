use super::*;

#[test]
fn close_at_root_cursor_fails_without_mutating_parse_stack() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "root child work");
    close_task(&mut runtime, &mut raw, "close-1-1", "1.1");
    let before = runtime.parse_stack().clone();
    let (_, request_raw, request_context) =
        observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_CLOSE, "close-root");
    let err = runtime
        .stage_close("close-root".to_string(), "test node memory".to_string())
        .expect_err("root cursor close should fail at stage time");
    assert!(
        err.to_string().contains("cannot close root epoch cursor 1"),
        "unexpected root close error: {err}"
    );
    assert!(
        runtime
            .pending_commit("close-root")
            .expect("pending lookup after rejected close")
            .is_none(),
        "rejected root close must not install pending close intent"
    );
    assert_eq!(runtime.parse_stack(), &before);
    let (_, response_raw, response_context) =
        observe_function_output(&mut runtime, &mut raw, "close-root");
    let aborted_pending = runtime
        .commit_completed_toolcall_as_ordinary_with_raw_items(
            "close-root",
            single_request_response_toolcall(
                "close-root",
                request_raw,
                request_context,
                response_raw,
                response_context,
            ),
            &raw,
        )
        .expect("commit rejected close transaction as ordinary toolcall");
    assert!(
        !aborted_pending,
        "invalid close must not consume a pending close symbol"
    );
    assert!(matches!(
        runtime.parse_stack().symbols.as_slice(),
        [
            Symbol::Control(ControlSymbol::Init(_)),
            Symbol::SpineTreeNodes(nodes),
        ] if matches!(
            nodes.as_slice(),
            [
                SpineTreeNode::SpineTree { meta, .. },
                SpineTreeNode::ToolCallAsLeafNode { segments: close_segments },
                SpineTreeNode::ToolCallAsLeafNode { segments: rejected_segments },
            ] if meta.id == NodeId::root_epoch(1).child(1)
                && close_segments == &vec![tool_req(1, 1), tool_resp(2, 2)]
                && rejected_segments == &vec![
                    tool_req(request_raw, request_context),
                    tool_resp(response_raw, response_context),
                ]
        )
    ));
}
