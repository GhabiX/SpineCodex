use super::*;

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
        .materialize_variable_context_for_test(&raw)
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
        .materialize_variable_context_for_test(&raw)
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
