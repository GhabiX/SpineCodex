use super::*;

#[test]
fn layer_1_2_4_example_trace_replays_shift_reduce() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "root work");
    open_task(&mut runtime, &mut raw, "open-1-1", "task 1.1");
    append_msg(&mut runtime, &mut raw, "1.1 work");
    close_task(&mut runtime, &mut raw, "close-1-1", "1.1.1");
    open_task(&mut runtime, &mut raw, "open-1-2", "task 1.2");
    append_msg(&mut runtime, &mut raw, "1.2 work");
    open_task(&mut runtime, &mut raw, "open-1-2-1", "task 1.2.1");
    append_msg(&mut runtime, &mut raw, "1.2.1 work");
    close_task(&mut runtime, &mut raw, "close-1-2-1", "1.1.2.1");
    open_task(&mut runtime, &mut raw, "open-1-2-2", "task 1.2.2");
    append_msg(&mut runtime, &mut raw, "1.2.2 work");
    close_task(&mut runtime, &mut raw, "close-1-2-2", "1.1.2.2");
    close_task(&mut runtime, &mut raw, "close-1-2", "1.1.2");
    append_msg(&mut runtime, &mut raw, "1.3 work");
    runtime
        .root_compact("root epoch 1 memory".to_string(), &raw)
        .expect("root compact");
    let post_compact_len = runtime
        .materialize_history(&raw)
        .expect("post-compact h(PS)")
        .len();
    append_msg(&mut runtime, &mut raw, "2.1 work");

    assert!(matches!(
        runtime.parse_stack().symbols.as_slice(),
        [
            Symbol::Control(ControlSymbol::Init(_)),
            Symbol::RootEpoches(root_epochs),
            Symbol::Control(ControlSymbol::Open(next_root)),
            Symbol::SpineTreeNodes(nodes),
        ] if root_epochs.len() == 1
            && root_epochs[0].memory.node_id == NodeId::root_epoch(1)
            && next_root.id == NodeId::root_epoch(2).child(1)
            && next_root.index == post_compact_len
            && matches!(
                nodes.as_slice(),
                [
                    SpineTreeNode::MsgAsLeafNode {
                        msg: SegRef::ResponseItem {
                            raw_ordinal,
                            context_index,
                        },
                        ..
                    }
                ] if *raw_ordinal == u64::try_from(raw.len() - 1).expect("ordinal")
                    && *context_index == post_compact_len
            )
    ));

    let replayed = SpineRuntime::load_for_rollout(&rollout, runtime.raw_len)
        .expect("load spine")
        .expect("sidecar exists");
    assert_eq!(
        replayed.parse_stack().symbols,
        runtime.parse_stack().symbols
    );

    let tree = replayed.parse_stack().render_tree().expect("render tree");
    assert!(tree.contains("[1] Done"), "{tree}");
    assert!(tree.contains("[2.1] Current"), "{tree}");
    assert!(
        !tree.contains("[1.2.1]") && !tree.contains("[1.2.2]"),
        "closed descendants of a previous root epoch must stay folded: {tree}"
    );

    let materialized = replayed.materialize_history(&raw).expect("materialize");
    assert_eq!(materialized.len(), 2);
    assert!(matches!(
        &materialized[0],
        ResponseItem::Message { content, .. }
            if matches!(
                content.as_slice(),
                [ContentItem::InputText { text }]
                    if text.contains("root epoch 1 memory")
            )
    ));
    assert_eq!(materialized[1], anchored_text_item(7, "2.1 work"));
}

// Native root compact and root epoch behavior.

#[test]
fn native_compact_shifts_compact_and_new_root_open() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw = vec![
        Some(text_item("before compact")),
        Some(text_item("more context")),
    ];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    runtime.observe_raw_items(2).expect("record raw");
    runtime
        .observe_context_item(0, 0, &text_item("before compact"))
        .expect("observe first context item");
    runtime
        .observe_context_item(1, 1, &text_item("more context"))
        .expect("observe second context item");

    runtime
        .root_compact("root summary".to_string(), &raw)
        .expect("compact root");

    let events = event_log(&runtime);
    assert!(matches!(
        events.as_slice(),
        [
            SpineLedgerEvent::Init { .. },
            SpineLedgerEvent::Open { summary, .. },
            SpineLedgerEvent::Msg { raw_ordinal: 0, .. },
            SpineLedgerEvent::Msg { raw_ordinal: 1, .. },
            SpineLedgerEvent::RootCompact {
                boundary: 2,
                next_open_index: 1,
                ..
            },
        ] if summary == "root"
    ));
    assert!(matches!(
        runtime.parse_stack().symbols.as_slice(),
        [
            Symbol::Control(ControlSymbol::Init(_)),
            Symbol::RootEpoches(root_epochs),
            Symbol::Control(ControlSymbol::Open(next_root)),
        ] if root_epochs.len() == 1
            && root_epochs[0].memory.node_id == NodeId::root_epoch(1)
            && root_epochs[0].memory.compact_id == "root-1-2"
            && next_root.id == NodeId::root_epoch(2).child(1)
            && next_root.index == 1
            && next_root.summary == "root"
    ));

    let replayed = SpineRuntime::load_for_rollout(&rollout, runtime.raw_len)
        .expect("load spine")
        .expect("sidecar exists");
    assert_eq!(
        replayed.parse_stack().symbols,
        runtime.parse_stack().symbols
    );
}

#[test]
fn root_compact_prepare_store_failure_retains_retryable_compact_without_events() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(
        &mut runtime,
        &mut raw,
        "context before root compact failure",
    );
    append_msg(
        &mut runtime,
        &mut raw,
        "more context before root compact failure",
    );
    let before_events = ledger_event_debug(&runtime);
    let blocked_root = dir.path().join("not-a-dir-root-compact");
    std::fs::write(&blocked_root, "file blocks sidecar dir").expect("write blocker file");
    runtime.store.root = blocked_root;

    runtime
        .prepare_root_compact_with_checkpoint(
            &rollout,
            "root compact memory that will fail before commit".to_string(),
            &raw,
            SpineRootCompactTokenMetadata::default(),
        )
        .expect_err("root compact prepare must fail while writing sidecar memory");
    assert_pending_compact_retry_state(&runtime, &before_events);
}

#[test]
fn root_depth_open_after_native_compact_can_close_and_open_sibling() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "epoch one work");
    runtime
        .root_compact("root summary".to_string(), &raw)
        .expect("compact root");
    let _post_compact_len = runtime
        .materialize_history(&raw)
        .expect("materialize")
        .len();

    append_msg(&mut runtime, &mut raw, "epoch two child work");
    close_task(&mut runtime, &mut raw, "close-2-1", "2.1");

    let tree = runtime.render_tree().expect("render tree");
    assert!(tree.contains("Cursor: 2"), "{tree}");
    assert!(tree.contains("[1] Done"), "{tree}");
    assert!(tree.contains("[2] Current"), "{tree}");
    assert!(tree.contains("[2.1] Done"), "{tree}");

    let post_close_len = runtime
        .materialize_history(&raw)
        .expect("materialize after close")
        .len();
    open_task(&mut runtime, &mut raw, "open-2-2", "task 2.2");
    assert!(matches!(
        runtime.parse_stack().symbols.as_slice(),
        [
            Symbol::Control(ControlSymbol::Init(_)),
            Symbol::RootEpoches(root_epochs),
            Symbol::SpineTreeNodes(_),
            Symbol::Control(ControlSymbol::Open(open)),
            Symbol::SpineTreeNodes(open_nodes),
        ] if root_epochs.len() == 1
            && open.id == NodeId::root_epoch(2).child(2)
            && open.index == post_close_len
            && open.summary == "task 2.2"
            && matches!(
                open_nodes.as_slice(),
                [SpineTreeNode::ToolCallAsLeafNode { segments }]
                    if segments == &vec![tool_req(4, 4), tool_resp(5, 5)]
            )
    ));
}

#[test]
fn prepare_root_compact_does_not_install_final_parse_stack() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "before staged root compact");
    append_msg(&mut runtime, &mut raw, "more staged root compact context");
    let before_tree = runtime
        .render_tree()
        .expect("render before prepared root compact");
    let before_snapshot = runtime
        .build_tree_snapshot()
        .expect("snapshot before prepared root compact");

    let prepared = runtime
        .prepare_root_compact_with_checkpoint(
            &rollout,
            "staged root compact body".to_string(),
            &raw,
            SpineRootCompactTokenMetadata::default(),
        )
        .expect("prepare root compact");

    assert_eq!(
        runtime
            .render_tree()
            .expect("render after prepared root compact"),
        before_tree,
        "prepared root compact must not install the reduced ParseStack before host publication"
    );
    let staged_snapshot = runtime
        .build_tree_snapshot()
        .expect("snapshot after prepared root compact");
    assert_eq!(
        staged_snapshot.active_node_id, before_snapshot.active_node_id,
        "prepared root compact must not advance the live active node"
    );

    runtime.install_prepared_root_compact(prepared);
    let after_snapshot = runtime
        .build_tree_snapshot()
        .expect("snapshot after installing prepared root compact");
    assert_ne!(
        after_snapshot.active_node_id, before_snapshot.active_node_id,
        "installing prepared root compact should advance the live ParseStack"
    );
}
