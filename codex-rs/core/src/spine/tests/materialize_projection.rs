use super::*;

#[test]
fn tree_renders_from_parse_stack_without_mutating_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 1).expect("create spine");

    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(1, 1, &spine_call(SPINE_TOOL_OPEN, "open"))
        .expect("observe open request");
    runtime
        .stage_open("open".to_string(), "child task".to_string())
        .expect("stage open");
    runtime.observe_raw_items(1).expect("record open output");
    runtime
        .observe_context_item(2, 2, &function_output("open"))
        .expect("observe open output");
    runtime
        .maybe_commit_output("open", None)
        .expect("commit open");
    runtime.observe_raw_items(1).expect("observe child item");
    runtime
        .observe_context_item(3, 3, &text_item("inside"))
        .expect("observe child item");
    runtime.observe_raw_items(1).expect("record close request");
    runtime
        .observe_context_item(4, 4, &spine_call(SPINE_TOOL_CLOSE, "close"))
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
        .observe_context_item(5, 5, &function_output("close"))
        .expect("observe close output");
    runtime
        .maybe_commit_output(
            "close",
            Some(memory_assembly_with_ranges("1.1.1", suffix_start..4, 1..4)),
        )
        .expect("commit close");

    let replayed = SpineRuntime::load_for_rollout(&rollout, runtime.raw_len)
        .expect("load spine")
        .expect("sidecar exists");
    let before = replayed.parse_stack().clone();
    let tree = replayed.render_tree().expect("render tree");
    assert_eq!(replayed.parse_stack(), &before);
    assert_eq!(
        tree,
        replayed.parse_stack().render_tree().expect("render ps")
    );
    assert!(tree.contains("Cursor: 1.1"), "{tree}");
    assert!(tree.contains("Spine Task Tree:"), "{tree}");
    assert!(tree.contains("[1.1] Current"), "{tree}");
    assert!(tree.contains("[1.1.1] Done child task"), "{tree}");
    assert!(
        tree.contains("memory=nodes/1/1/1/Memory.md")
            && tree.contains("trajs=nodes/1/1/1/Trajs.md"),
        "{tree}"
    );
}

#[test]
fn materialize_history_renders_from_parse_stack_memory_segments() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw = vec![
        Some(text_item("before")),
        Some(spine_call(SPINE_TOOL_OPEN, "open")),
        Some(function_output("open")),
        Some(text_item("inside")),
        Some(spine_call(SPINE_TOOL_CLOSE, "close")),
        Some(function_output("close")),
    ];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 1).expect("create spine");
    runtime
        .observe_context_item(0, 0, &text_item("before"))
        .expect("observe prefix");
    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(1, 1, &spine_call(SPINE_TOOL_OPEN, "open"))
        .expect("observe open request");
    runtime
        .stage_open("open".to_string(), "child task".to_string())
        .expect("stage open");
    runtime.observe_raw_items(1).expect("record open output");
    runtime
        .observe_context_item(2, 2, &function_output("open"))
        .expect("observe open output");
    runtime
        .maybe_commit_output("open", None)
        .expect("commit open");
    runtime.observe_raw_items(1).expect("observe child item");
    runtime
        .observe_context_item(3, 3, &text_item("inside"))
        .expect("observe child item");
    runtime.observe_raw_items(1).expect("record close request");
    runtime
        .observe_context_item(4, 4, &spine_call(SPINE_TOOL_CLOSE, "close"))
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
        .observe_context_item(5, 5, &function_output("close"))
        .expect("observe close output");
    runtime
        .maybe_commit_output(
            "close",
            Some(memory_assembly_with_ranges("1.1.1", suffix_start..4, 1..4)),
        )
        .expect("commit close");

    let Some(Symbol::SpineTreeNodes(nodes)) = runtime.parse_stack().symbols.last() else {
        panic!("closed child should reduce into ParseStack nodes")
    };
    let memory = nodes
        .iter()
        .find_map(|node| match node {
            SpineTreeNode::SpineTree { memory, .. } => Some(memory),
            _ => None,
        })
        .expect("closed child memory ref");
    assert_eq!(memory.compact_id, "mem-1-1-1-1-4");
    assert_eq!(memory.source_context_range, 1..4);
    assert_eq!(memory.source_raw_range, 1..4);
    let memory_seg = SegRef::from_memory_ref(memory);
    assert!(matches!(
        &memory_seg,
        SegRef::Memory {
            memory_id,
            body_path,
        } if memory_id == "mem-1-1-1-1-4"
            && body_path.ends_with("memory/mem-1-1-1-1-4.md")
    ));
    let memory_only = ParseStack {
        symbols: vec![Symbol::SpineTreeNodes(vec![SpineTreeNode::MsgAsLeafNode {
            msg: memory_seg,
            from_user: true,
            user_anchor: None,
        }])],
    };
    let rendered_memory =
        render_parse_stack_to_context(&memory_only, &[]).expect("render SegRef::Memory");
    assert!(matches!(
        rendered_memory.as_slice(),
        [ResponseItem::Message { content, .. }]
            if matches!(
                content.as_slice(),
                [ContentItem::InputText { text }]
                    if text.contains("Spine Memory 1.1.1")
                        && text.contains("real compact body for 1.1.1")
            )
    ));

    let materialized = runtime.materialize_history(&raw).expect("materialize");
    assert_eq!(materialized.len(), 4);
    assert_eq!(materialized[0], anchored_text_item(1, "before"));
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
    assert_eq!(materialized[2], spine_call(SPINE_TOOL_CLOSE, "close"));
    assert_eq!(materialized[3], function_output("close"));
}
