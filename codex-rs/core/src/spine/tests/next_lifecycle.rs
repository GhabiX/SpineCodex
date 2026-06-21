use super::*;

// Root-depth lifecycle and spine.next transactions.

#[test]
fn root_depth_open_node_can_close_and_next_open_creates_sibling() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "root child work");
    close_task(&mut runtime, &mut raw, "close-1-1", "1.1");

    let tree = runtime.render_tree().expect("render tree");
    assert!(tree.contains("Cursor: 1"), "{tree}");
    assert!(tree.contains("[1] Current"), "{tree}");
    assert!(tree.contains("[1.1] Done"), "{tree}");
    assert!(!tree.contains("root"), "{tree}");

    let materialized = runtime.materialize_history(&raw).expect("materialize");
    assert_eq!(materialized.len(), 3);
    assert!(matches!(
        &materialized[0],
        ResponseItem::Message { content, .. }
            if matches!(
                content.as_slice(),
                [ContentItem::InputText { text }]
                    if text.contains("Spine Memory 1.1")
                        && text.contains("real compact body for 1.1")
            )
    ));
    assert_eq!(materialized[1], spine_call(SPINE_TOOL_CLOSE, "close-1-1"));
    assert_eq!(materialized[2], function_output("close-1-1"));

    let snapshot = runtime.build_tree_snapshot().expect("snapshot");
    assert_snapshot_is_self_contained_forest(&snapshot);
    let nodes = snapshot_nodes_by_id(&snapshot);
    assert_eq!(snapshot.active_node_id, "1");
    assert_eq!(nodes["1"].status, SpineTreeNodeStatus::Live);
    assert_eq!(nodes["1.1"].parent_id.as_deref(), Some("1"));
    assert_eq!(nodes["1.1"].status, SpineTreeNodeStatus::Closed);

    open_task(&mut runtime, &mut raw, "open-1-2", "task 1.2");
    assert!(matches!(
        runtime.parse_stack().symbols.as_slice(),
        [
            Symbol::Control(ControlSymbol::Init(_)),
            Symbol::SpineTreeNodes(_),
            Symbol::Control(ControlSymbol::Open(open)),
            Symbol::SpineTreeNodes(open_nodes),
        ] if open.id == NodeId::root_epoch(1).child(2)
            && open.summary == "task 1.2"
            && matches!(
                open_nodes.as_slice(),
                [SpineTreeNode::ToolCallAsLeafNode { segments }]
                    if segments == &vec![tool_req(3, 3), tool_resp(4, 4)]
            )
    ));
}

#[test]
fn checkpoint_after_root_depth_close_records_root_cursor() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "root child work");
    close_task(&mut runtime, &mut raw, "close-1-1", "1.1");
    let context = runtime.materialize_history(&raw).expect("materialize");

    runtime
        .checkpoint_before_user_msg(&rollout, runtime.raw_len, &raw)
        .expect("write root cursor checkpoint");
    let checkpoint = runtime
        .store
        .checkpoint_for_test(runtime.raw_len)
        .expect("read root cursor checkpoint");

    assert_eq!(checkpoint.cursor, "1");
    assert_eq!(
        checkpoint.h_ps_hash,
        hash_response_items(&context).expect("hash root cursor context")
    );
    assert!(matches!(
        checkpoint.parse_stack.symbols.as_slice(),
        [
            Symbol::Control(ControlSymbol::Init(_)),
            Symbol::SpineTreeNodes(nodes),
        ] if nodes.len() == 2 && matches!(
            nodes.as_slice(),
            [
                SpineTreeNode::SpineTree { meta, .. },
                SpineTreeNode::ToolCallAsLeafNode { segments },
            ]
                if meta.id == NodeId::root_epoch(1).child(1)
                    && segments == &vec![tool_req(1, 1), tool_resp(2, 2)]
        )
    ));
}

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
            completed_toolcall(
                "close-root",
                vec![
                    tool_req(request_raw, request_context),
                    tool_resp(response_raw, response_context),
                ],
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

#[test]
fn next_at_root_cursor_fails_without_pending_transition() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "root child work");
    close_task(&mut runtime, &mut raw, "close-1-1", "1.1");
    let before = runtime.parse_stack().clone();
    observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_NEXT, "next-root");
    let err = runtime
        .stage_next(
            "next-root".to_string(),
            "must not open sibling".to_string(),
            "test node memory".to_string(),
        )
        .expect_err("root cursor next should fail at stage time");
    assert!(
        err.to_string().contains("cannot close root epoch cursor 1"),
        "unexpected root next error: {err}"
    );
    assert!(
        runtime
            .pending_commit("next-root")
            .expect("pending lookup after rejected next")
            .is_none(),
        "rejected root next must not install pending close/open intent"
    );
    assert_eq!(runtime.parse_stack(), &before);
}
