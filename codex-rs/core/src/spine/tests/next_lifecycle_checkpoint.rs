use super::*;

#[test]
fn checkpoint_after_root_depth_close_records_root_cursor() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "root child work");
    close_task(&mut runtime, &mut raw, "close-1-1", "1.1");
    let context = runtime.materialize_history_for_test(&raw).expect("materialize");

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
