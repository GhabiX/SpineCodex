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
fn spine_next_equivalent_to_close_then_open() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "root child work");
    open_task(&mut runtime, &mut raw, "open-child", "nested child");
    append_msg(&mut runtime, &mut raw, "nested child work");

    let commit = next_task(
        &mut runtime,
        &mut raw,
        "next-child",
        "1.1.1",
        "next sibling",
    );

    assert!(matches!(
        commit,
        SpineCommitKind::CloseThenOpen { open_index: 2 }
    ));
    assert!(matches!(
        runtime.parse_stack().symbols.as_slice(),
        [
            Symbol::Control(ControlSymbol::Init(_)),
            Symbol::Control(ControlSymbol::Open(root_child)),
            Symbol::SpineTreeNodes(_),
            Symbol::Control(ControlSymbol::Open(next_sibling)),
            Symbol::SpineTreeNodes(next_nodes),
        ] if root_child.id == NodeId::root_epoch(1).child(1)
            && next_sibling.id == NodeId::root_epoch(1).child(1).child(2)
            && next_sibling.summary == "next sibling"
            && next_sibling.index == 2
            && next_sibling.open_context_tokens.is_none()
            && next_sibling.open_input_tokens.is_none()
            && matches!(
                next_nodes.as_slice(),
                [SpineTreeNode::ToolCallAsLeafNode { segments }]
                    if segments == &vec![tool_req(4, 2), tool_resp(5, 3)]
            )
    ));

    let events = event_log(&runtime);
    assert_eq!(runtime.ledger.next_event_seq, 9);
    assert!(
        events
            .iter()
            .all(|event| !matches!(event, SpineLedgerEvent::RootCompact { .. }))
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, SpineLedgerEvent::Close { .. }))
            .count(),
        1
    );
    assert!(matches!(
        events.as_slice(),
        [
            SpineLedgerEvent::Init { .. },
            SpineLedgerEvent::Open { child: initial, .. },
            SpineLedgerEvent::Msg { raw_ordinal: 0, .. },
            SpineLedgerEvent::Open { child: nested, .. },
            SpineLedgerEvent::ToolCall { .. },
            SpineLedgerEvent::Msg { raw_ordinal: 3, .. },
            SpineLedgerEvent::Close { node: closed, .. },
            SpineLedgerEvent::Open {
                child: next,
                index,
                summary,
                open_input_tokens: None,
                open_context_tokens: None,
                open_context_source: None,
                ..
            },
            SpineLedgerEvent::ToolCall { .. },
        ] if *initial == NodeId::root_epoch(1).child(1)
            && *nested == NodeId::root_epoch(1).child(1).child(1)
            && *closed == NodeId::root_epoch(1).child(1).child(1)
            && *next == NodeId::root_epoch(1).child(1).child(2)
            && *index == 2
            && summary == "next sibling"
    ));

    let materialized = runtime.materialize_history(&raw).expect("materialize");
    assert_eq!(materialized.len(), 4);
    assert!(matches!(
        &materialized[1],
        ResponseItem::Message { content, .. }
            if matches!(
                content.as_slice(),
                [ContentItem::InputText { text }]
                    if text.contains("Spine Memory 1.1.1")
                        && text.contains("real compact body for 1.1.1")
            )
    ));
    assert_eq!(materialized[2], spine_call(SPINE_TOOL_NEXT, "next-child"));
    assert_eq!(materialized[3], function_output("next-child"));
}

#[test]
fn spine_next_defers_sibling_open_provider_baseline_until_post_replacement_usage() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "root child work");
    open_task(&mut runtime, &mut raw, "open-child", "nested child");
    append_msg(&mut runtime, &mut raw, "nested child work");

    let token_baselines = SpineTokenBaselines {
        provider_input_tokens: Some(12_345),
    };
    let commit = next_task_with_token_baselines(
        &mut runtime,
        &mut raw,
        "next-child",
        "1.1.1",
        "next sibling",
        token_baselines,
    );

    assert!(matches!(
        commit,
        SpineCommitKind::CloseThenOpen { open_index: 2, .. }
    ));
    assert_eq!(runtime.current_open_input_tokens(), None);
    assert_eq!(runtime.current_open_provider_input_tokens(), None);
    assert_eq!(runtime.current_open_context_baseline_source(), None);
    assert!(matches!(
        runtime.parse_stack().symbols.as_slice(),
        [
            Symbol::Control(ControlSymbol::Init(_)),
            Symbol::Control(ControlSymbol::Open(_)),
            Symbol::SpineTreeNodes(_),
            Symbol::Control(ControlSymbol::Open(next_sibling)),
            Symbol::SpineTreeNodes(next_nodes),
        ] if next_sibling.id == NodeId::root_epoch(1).child(1).child(2)
            && next_sibling.summary == "next sibling"
            && next_sibling.index == 2
            && next_sibling.open_input_tokens.is_none()
            && next_sibling.open_context_tokens.is_none()
            && next_sibling.open_context_source.is_none()
            && matches!(
                next_nodes.as_slice(),
                [SpineTreeNode::ToolCallAsLeafNode { segments }]
                    if segments == &vec![tool_req(4, 2), tool_resp(5, 3)]
            )
    ));

    let events = event_log(&runtime);
    assert!(matches!(
        events.as_slice(),
        [
            SpineLedgerEvent::Init { .. },
            SpineLedgerEvent::Open { .. },
            SpineLedgerEvent::Msg { .. },
            SpineLedgerEvent::Open { .. },
            SpineLedgerEvent::ToolCall { .. },
            SpineLedgerEvent::Msg { .. },
            SpineLedgerEvent::Close { .. },
            SpineLedgerEvent::Open {
                child: next,
                index: 2,
                open_input_tokens: None,
                open_context_tokens: None,
                open_context_source: None,
                ..
            },
            SpineLedgerEvent::ToolCall { .. },
        ] if *next == NodeId::root_epoch(1).child(1).child(2)
    ));

    runtime
        .capture_current_open_provider_baseline(7_913)
        .expect("capture post-replacement provider baseline for next sibling");
    assert_eq!(runtime.current_open_input_tokens(), Some(7_913));
    assert_eq!(runtime.current_open_provider_input_tokens(), Some(7_913));
    assert_eq!(
        runtime.current_open_context_baseline_source(),
        Some(SpineNodeContextBaselineSource::ProviderAtOpen)
    );
    assert!(matches!(
        event_log(&runtime).as_slice(),
        [
            ..,
            SpineLedgerEvent::OpenContextBaseline {
                node,
                open_input_tokens: 7_913,
                open_context_tokens: 7_913,
                open_context_source: ContextBaselineSource::ProviderAtOpen,
                ..
            },
        ] if *node == NodeId::root_epoch(1).child(1).child(2)
    ));

    let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw, &[])
        .expect("load spine")
        .expect("sidecar exists");
    assert_eq!(replayed.current_open_input_tokens(), Some(7_913));
    assert_eq!(replayed.current_open_provider_input_tokens(), Some(7_913));
    assert_eq!(
        replayed.current_open_context_baseline_source(),
        Some(SpineNodeContextBaselineSource::ProviderAtOpen)
    );
}

#[test]
fn spine_next_close_failure_does_not_open_sibling() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "root child work");
    open_task(&mut runtime, &mut raw, "open-child", "nested child");
    append_msg(&mut runtime, &mut raw, "nested child work");

    let request = spine_call(SPINE_TOOL_NEXT, "bad-next");
    let request_ordinal = u64::try_from(raw.len()).expect("raw ordinal fits u64");
    let request_context_index = current_context_len(&runtime, &raw);
    raw.push(Some(request.clone()));
    runtime.observe_raw_items(1).expect("record next request");
    runtime
        .observe_context_item(request_ordinal, request_context_index, &request)
        .expect("observe next request");
    runtime
        .stage_next(
            "bad-next".to_string(),
            "next sibling".to_string(),
            "test node memory".to_string(),
        )
        .expect("stage next");
    let output = function_output("bad-next");
    runtime.observe_raw_items(1).expect("record next output");
    raw.push(Some(output.clone()));
    runtime
        .observe_context_item(5, 5, &output)
        .expect("observe next output");

    let err = runtime
        .maybe_commit_output(
            "bad-next",
            Some(memory_assembly_with_context_range("1.1.1", 0..raw.len())),
        )
        .expect_err("bad compact range should fail next");
    assert!(
        err.to_string().contains("expected suffix start 1"),
        "unexpected next failure: {err}"
    );
    assert!(matches!(
        runtime.parse_stack().symbols.as_slice(),
        [
            Symbol::Control(ControlSymbol::Init(_)),
            Symbol::Control(ControlSymbol::Open(root_child)),
            Symbol::SpineTreeNodes(_),
            Symbol::Control(ControlSymbol::Open(nested)),
            Symbol::SpineTreeNodes(_),
        ] if root_child.id == NodeId::root_epoch(1).child(1)
            && nested.id == NodeId::root_epoch(1).child(1).child(1)
    ));
    assert!(
        event_log(&runtime)
            .iter()
            .all(|event| !matches!(event, SpineLedgerEvent::Close { .. }))
    );
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
