use super::*;

#[test]
fn task_tree_reduce_archive_failure_leaves_symbols_unchanged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let archive = SpineArchive::staged_with_memory_body(
        dir.path().to_path_buf(),
        "bad-memory".to_string(),
        "wrong body".to_string(),
    );
    let node_id = NodeId::root_epoch(1).child(1);
    let meta = tree_meta(&archive, node_id.clone(), 0, "child".to_string()).expect("meta");
    let memory = memory_ref(
        &archive,
        "bad-memory".to_string(),
        node_id,
        sha1_hex(b"expected body"),
        0..1,
        0..1,
        1..2,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    );
    let mut parse_stack = ParseStack {
        symbols: vec![
            Symbol::Control(ControlSymbol::Open(meta)),
            Symbol::SpineTreeNodes(vec![SpineTreeNode::MsgAsLeafNode {
                msg: SegRef::ResponseItem {
                    raw_ordinal: 0,
                    context_index: 0,
                },
                from_user: true,
                user_anchor: Some(1),
            }]),
            Symbol::Control(ControlSymbol::Close(memory)),
        ],
    };
    let before = parse_stack.symbols.clone();

    let err = parse_stack
        .shift(SpineToken::End, &archive)
        .expect_err("archive failure must abort close reduction");
    assert!(
        err.to_string().contains("staged memory body hash mismatch"),
        "unexpected archive failure: {err}"
    );
    assert_eq!(
        parse_stack.symbols, before,
        "failed close reduction must not pop/truncate the live symbols"
    );
}
