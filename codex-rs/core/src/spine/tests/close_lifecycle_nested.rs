use super::*;

#[test]
fn nested_close_reduces_inner_tree_into_parent_nodes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime
        .observe_raw_items(1)
        .expect("record outer open request");
    runtime
        .observe_context_item(0, 0, &spine_call(SPINE_TOOL_OPEN, "outer"))
        .expect("observe outer open request");
    runtime
        .stage_open("outer".to_string(), "outer".to_string())
        .expect("stage outer open");
    runtime.observe_raw_items(1).expect("record outer output");
    runtime
        .observe_context_item(1, 1, &function_output("outer"))
        .expect("observe outer output");
    runtime
        .maybe_commit_output("outer", None)
        .expect("commit outer");

    runtime
        .observe_raw_items(1)
        .expect("record inner open request");
    runtime
        .observe_context_item(2, 2, &spine_call(SPINE_TOOL_OPEN, "inner"))
        .expect("observe inner open request");
    runtime
        .stage_open("inner".to_string(), "inner".to_string())
        .expect("stage inner open");
    runtime.observe_raw_items(1).expect("record inner output");
    runtime
        .observe_context_item(3, 3, &function_output("inner"))
        .expect("observe inner output");
    runtime
        .maybe_commit_output("inner", None)
        .expect("commit inner");

    runtime.observe_raw_items(1).expect("record inner raw");
    runtime
        .observe_context_item(4, 4, &text_item("inner body"))
        .expect("observe inner raw");
    runtime
        .observe_raw_items(1)
        .expect("record inner close request");
    runtime
        .observe_context_item(5, 5, &spine_call(SPINE_TOOL_CLOSE, "close-inner"))
        .expect("observe inner close request");
    runtime
        .stage_close("close-inner".to_string(), "test node memory".to_string())
        .expect("stage inner close");
    let inner_suffix_start = match runtime
        .pending_commit("close-inner")
        .expect("pending inner close")
    {
        Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
        other => panic!("expected pending inner close, got {other:?}"),
    };
    runtime
        .observe_raw_items(1)
        .expect("record inner close output");
    runtime
        .observe_context_item(6, 6, &function_output("close-inner"))
        .expect("observe inner close output");
    runtime
        .maybe_commit_output(
            "close-inner",
            Some(memory_assembly_with_context_range(
                "1.1.1.1",
                inner_suffix_start..5,
            )),
        )
        .expect("commit inner close");

    assert!(matches!(
        runtime.parse_stack().symbols.as_slice(),
        [
            Symbol::Control(ControlSymbol::Init(_)),
            Symbol::Control(ControlSymbol::Open(root)),
            Symbol::Control(ControlSymbol::Open(outer)),
            Symbol::SpineTreeNodes(nodes),
        ] if root.id == NodeId::root_epoch(1).child(1)
            && outer.id == NodeId::root_epoch(1).child(1).child(1)
            && matches!(
                nodes.as_slice(),
                [
                    SpineTreeNode::ToolCallAsLeafNode { segments: outer_open_segments },
                    SpineTreeNode::SpineTree { meta, .. },
                    SpineTreeNode::ToolCallAsLeafNode { segments },
                ]
                    if outer_open_segments == &vec![tool_req(0, 0), tool_resp(1, 1)]
                        && meta.id == NodeId::root_epoch(1).child(1).child(1).child(1)
                        && meta.summary == "inner"
                        && segments == &vec![tool_req(5, 3), tool_resp(6, 4)]
            )
    ));

    runtime
        .observe_raw_items(1)
        .expect("record outer close request");
    runtime
        .observe_context_item(7, 7, &spine_call(SPINE_TOOL_CLOSE, "close-outer"))
        .expect("observe outer close request");
    runtime
        .stage_close("close-outer".to_string(), "test node memory".to_string())
        .expect("stage outer close");
    let outer_suffix_start = match runtime
        .pending_commit("close-outer")
        .expect("pending outer close")
    {
        Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
        other => panic!("expected pending outer close, got {other:?}"),
    };
    runtime
        .observe_raw_items(1)
        .expect("record outer close output");
    runtime
        .observe_context_item(8, 8, &function_output("close-outer"))
        .expect("observe outer close output");
    runtime
        .maybe_commit_output(
            "close-outer",
            Some(memory_assembly_with_context_range(
                "1.1.1",
                outer_suffix_start..7,
            )),
        )
        .expect("commit outer close");

    let Some(Symbol::SpineTreeNodes(root_nodes)) = runtime.parse_stack().symbols.last() else {
        panic!("outer close should reduce to root Nodes")
    };
    assert!(matches!(
        root_nodes.as_slice(),
        [
            SpineTreeNode::SpineTree {
                meta,
                children,
                trajs_path,
                ..
            },
            SpineTreeNode::ToolCallAsLeafNode { segments },
        ] if meta.id == NodeId::root_epoch(1).child(1).child(1)
            && meta.summary == "outer"
            && segments == &vec![tool_req(7, 1), tool_resp(8, 2)]
            && matches!(
                children.as_slice(),
                [
                    SpineTreeNode::ToolCallAsLeafNode { segments: outer_open_segments },
                    SpineTreeNode::SpineTree { meta: inner, children: inner_children, .. },
                    SpineTreeNode::ToolCallAsLeafNode { segments: inner_close_segments },
                ] if outer_open_segments == &vec![tool_req(0, 0), tool_resp(1, 1)]
                    && inner.summary == "inner"
                    && matches!(
                        inner_children.as_slice(),
                        [
                            SpineTreeNode::ToolCallAsLeafNode { segments },
                            SpineTreeNode::MsgAsLeafNode { .. },
                        ] if segments == &vec![tool_req(2, 2), tool_resp(3, 3)]
                    )
                    && inner_close_segments == &vec![tool_req(5, 3), tool_resp(6, 4)]
            )
            && trajs_path == &PathBuf::from("nodes/1/1/1/Trajs.md")
    ));
    let outer_trajs = std::fs::read_to_string(runtime.store.root.join("nodes/1/1/1/Trajs.md"))
        .expect("outer trajs");
    assert!(outer_trajs.contains("compact_id=mem-1-1-1-1-2-5"));
    assert!(outer_trajs.contains("node_id=1.1.1.1"));
    assert!(outer_trajs.contains("body_path="));
    assert!(outer_trajs.contains("memory_path=nodes/1/1/1/1/Memory.md"));
    assert!(outer_trajs.contains("trajs_path=nodes/1/1/1/1/Trajs.md"));
    assert!(!outer_trajs.contains("body_hash:"));
    assert!(!outer_trajs.contains("body:"));
    assert!(!outer_trajs.contains("Spine Memory 1.1.1.1"));
    assert!(!outer_trajs.contains("inner assistant traj"));
}
