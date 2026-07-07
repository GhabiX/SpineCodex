use super::*;

#[test]
fn root_compact_native_history_renders_original_message_slots() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "pre-compact work");
    let replacement_history = vec![
        text_item("retained user message"),
        text_item("Conversation summary:\nfirst native compact summary"),
    ];
    let root_body =
        serde_json::to_string_pretty(&replacement_history).expect("serialize replacement history");

    runtime.root_compact(root_body, &raw).expect("root compact");
    append_msg(&mut runtime, &mut raw, "post-compact work");

    let materialized = runtime
        .materialize_variable_context_for_test(&raw)
        .expect("materialize h(PS)");
    assert_eq!(materialized.len(), 3);
    assert_eq!(materialized[0], replacement_history[0]);
    assert_eq!(materialized[1], replacement_history[1]);
    assert_eq!(materialized[2], anchored_text_item(2, "post-compact work"));
    assert!(
        materialized
            .iter()
            .all(|item| !response_item_trace_signature(item).contains("<spine_memory>")),
        "native root compact memory must not be wrapped: {materialized:#?}"
    );
}

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
        .materialize_variable_context_for_test(&raw)
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

    let materialized = replayed
        .materialize_variable_context_for_test(&raw)
        .expect("materialize");
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
