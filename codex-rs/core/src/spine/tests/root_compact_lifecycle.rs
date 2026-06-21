use super::*;

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
