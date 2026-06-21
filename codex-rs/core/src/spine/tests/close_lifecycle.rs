use super::*;

#[test]
fn close_retry_reduces_existing_pending_close_token() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    open_task(&mut runtime, &mut raw, "open", "child");
    append_msg(&mut runtime, &mut raw, "inside");
    observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_CLOSE, "close-retry");
    runtime
        .stage_close("close-retry".to_string(), "test node memory".to_string())
        .expect("stage close");
    let suffix_start = match runtime
        .pending_commit("close-retry")
        .expect("pending close")
    {
        Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
        other => panic!("expected pending close, got {other:?}"),
    };
    let close_request_index = current_context_len(&runtime, &raw) - 1;
    observe_function_output(&mut runtime, &mut raw, "close-retry");
    let memory_assembly =
        memory_assembly_with_context_range("1.1.1", suffix_start..close_request_index);

    let prepared_memory = runtime
        .prepared_close_memory_for_test(
            Some(memory_assembly.clone()),
            SpineTokenBaselines::default(),
        )
        .expect("prepare close commit");
    runtime
        .parse_stack
        .shift_pending_close(prepared_memory, &runtime.archive())
        .expect("simulate retryable pending Close token");
    assert!(matches!(
        runtime.parse_stack().symbols.as_slice(),
        [
            ..,
            Symbol::Control(ControlSymbol::Open(_)),
            Symbol::SpineTreeNodes(_),
            Symbol::Control(ControlSymbol::Close(_))
        ]
    ));

    let commit = runtime
        .maybe_commit_output("close-retry", Some(memory_assembly))
        .expect("retry close")
        .expect("close should commit on retry");
    assert!(matches!(commit, SpineCommitKind::Close { .. }));
    assert!(!matches!(
        runtime.parse_stack().symbols.as_slice(),
        [
            ..,
            Symbol::Control(ControlSymbol::Open(_)),
            Symbol::SpineTreeNodes(_),
            Symbol::Control(ControlSymbol::Close(_))
        ]
    ));
    assert_eq!(
        runtime
            .store
            .commit_markers_for_test()
            .expect("commit markers")
            .len(),
        1
    );
}

#[test]
fn close_retry_reuses_matching_prepared_memory() {
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
    let close_request_index = 3;
    runtime.observe_raw_items(1).expect("record close output");
    runtime
        .observe_context_item(4, 4, &function_output("close"))
        .expect("observe close output");

    let memory_assembly =
        memory_assembly_with_context_range("1.1.1", suffix_start..close_request_index);
    let compact_id = "mem-1-1-1-0-3";
    let prepared_mem = MemRecord {
        compact_id: compact_id.to_string(),
        kind: MemKind::Suffix,
        node: NodeId(vec![1, 1, 1]),
        raw_start: 0,
        raw_end: 3,
        context_start: suffix_start,
        context_end: close_request_index,
        raw_live_hash: None,
        open_input_tokens: None,
        close_input_tokens: None,
        open_context_tokens: None,
        close_context_tokens: None,
        closed_source_suffix_tokens: None,
        closed_memory_context_tokens: None,
        open_context_source: None,
        memory_output_tokens: memory_assembly.memory_output_tokens,
        body_path: format!("memory/{compact_id}.md"),
        body_hash: sha1_hex(memory_assembly.body.as_bytes()),
    };
    runtime
        .store
        .write_memory_body(&prepared_mem.compact_id, &memory_assembly.body)
        .expect("write prepared memory body");
    runtime
        .store
        .append_mem(&prepared_mem)
        .expect("append prepared mem");

    let commit = runtime
        .maybe_commit_output("close", Some(memory_assembly))
        .expect("retry close with matching prepared memory")
        .expect("close should commit");
    assert!(matches!(commit, SpineCommitKind::Close { .. }));
    assert_eq!(
        runtime.store.mems().expect("read mems after retry").len(),
        1,
        "retry must reuse matching suffix mem instead of appending duplicate"
    );
    assert_eq!(
        runtime
            .store
            .commit_markers_for_test()
            .expect("read commit markers")
            .len(),
        1,
        "retry should publish the explicit close commit proof"
    );
    assert!(
        runtime
            .pending_commit("close")
            .expect("pending close")
            .is_none(),
        "successful retry must clear pending close"
    );
}

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
