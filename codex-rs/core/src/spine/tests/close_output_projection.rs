use super::*;

#[test]
fn spine_close_output_does_not_shift_msg() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(0, 0, &spine_call(SPINE_TOOL_OPEN, "open"))
        .expect("observe open request");
    runtime
        .stage_open("open".to_string(), "child".to_string())
        .expect("stage open");
    runtime.observe_raw_items(1).expect("record open output");
    runtime
        .observe_context_item(1, 1, &function_output("open"))
        .expect("observe open output");
    runtime
        .maybe_commit_output("open", None)
        .expect("commit open");

    runtime.observe_raw_items(1).expect("record child raw");
    runtime
        .observe_context_item(2, 2, &text_item("inside"))
        .expect("observe child raw");
    runtime.observe_raw_items(1).expect("record close request");
    runtime
        .observe_context_item(3, 3, &spine_call(SPINE_TOOL_CLOSE, "close"))
        .expect("observe close request");
    runtime
        .stage_close("close".to_string(), "test node memory".to_string())
        .expect("stage close");
    let suffix_start = match runtime.pending_commit("close").expect("pending close") {
        Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
        other => panic!("expected pending close, got {other:?}"),
    };
    runtime.observe_raw_items(1).expect("record close output");
    runtime
        .observe_context_item(4, 4, &function_output("close"))
        .expect("observe close output");
    runtime
        .maybe_commit_output(
            "close",
            Some(memory_assembly_with_ranges("1.1.1", suffix_start..4, 0..3)),
        )
        .expect("commit close");

    let events = event_log(&runtime);
    assert_eq!(
        events
            .iter()
            .filter_map(|event| match event {
                SpineLedgerEvent::Msg { raw_ordinal, .. } => Some(*raw_ordinal),
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec![2],
        "only the real child suffix item should shift as Msg"
    );
    assert!(
        !events.iter().any(|event| matches!(
            event,
            SpineLedgerEvent::Msg {
                raw_ordinal: 3 | 4,
                ..
            }
        )),
        "close request/output carriers must not shift as Msg"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, SpineLedgerEvent::Close { .. }))
            .count(),
        1
    );
    assert!(matches!(
        events.last(),
        Some(SpineLedgerEvent::ToolCall { .. })
    ));
    let Some(Symbol::SpineTreeNodes(nodes)) = runtime.parse_stack().symbols.last() else {
        panic!("close should reduce task tree into a tree node inside Nodes")
    };
    assert_eq!(nodes.len(), 2);
    let SpineTreeNode::SpineTree {
        meta,
        children,
        memory_path,
        trajs_path,
        ..
    } = &nodes[0]
    else {
        panic!("close should reduce to SpineTree")
    };
    assert!(matches!(
        &nodes[1],
        SpineTreeNode::ToolCallAsLeafNode { segments }
            if segments == &vec![tool_req(3, 1), tool_resp(4, 2)]
    ));
    assert_eq!(meta.id, NodeId::root_epoch(1).child(1).child(1));
    assert_eq!(meta.index, 0);
    assert_eq!(meta.summary, "child");
    assert!(matches!(
        children.as_slice(),
        [
            SpineTreeNode::ToolCallAsLeafNode {
                segments,
            },
            SpineTreeNode::MsgAsLeafNode {
                msg: SegRef::ResponseItem {
                    raw_ordinal: 2,
                    context_index: 2,
                },
                ..
            },
        ] if segments == &vec![tool_req(0, 0), tool_resp(1, 1)]
    ));
    assert_eq!(memory_path, &PathBuf::from("nodes/1/1/1/Memory.md"));
    assert_eq!(trajs_path, &PathBuf::from("nodes/1/1/1/Trajs.md"));

    let memory_archive =
        std::fs::read_to_string(runtime.store.root.join(memory_path)).expect("memory archive");
    assert!(memory_archive.contains("compact_id: mem-1-1-1-0-3"));
    assert!(memory_archive.contains("source_context_range: [0..4)"));
    assert!(memory_archive.contains("# Spine Memory 1.1.1"));
    let trajs_archive =
        std::fs::read_to_string(runtime.store.root.join(trajs_path)).expect("trajs archive");
    assert!(trajs_archive.contains("raw raw_ordinal=2 context_index=2"));
}
