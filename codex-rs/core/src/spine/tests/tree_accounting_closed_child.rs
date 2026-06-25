use super::*;

#[test]
fn closed_child_tree_records_raw_and_memory_context_accounting() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw = vec![
        Some(spine_call(SPINE_TOOL_OPEN, "open")),
        Some(function_output("open")),
        Some(text_item("inside")),
        Some(spine_call(SPINE_TOOL_CLOSE, "close")),
        Some(function_output("close")),
    ];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(0, 0, &spine_call(SPINE_TOOL_OPEN, "open"))
        .expect("observe open request");
    runtime
        .stage_open("open".to_string(), "accounted child".to_string())
        .expect("stage open");
    runtime.observe_raw_items(1).expect("record open output");
    runtime
        .observe_context_item(1, 1, &function_output("open"))
        .expect("observe open output");
    runtime
        .maybe_commit_output_with_open_input_tokens("open", None, Some(10_000))
        .expect("commit open");

    runtime.observe_raw_items(1).expect("observe child item");
    runtime
        .observe_context_item(2, 2, &text_item("inside"))
        .expect("observe child item");
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
    runtime
        .maybe_commit_output_with_open_input_tokens(
            "close",
            Some(memory_assembly_with_context_range(
                "1.1.1",
                suffix_start..close_request_index,
            )),
            Some(17_500),
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
    assert_eq!(memory.open_input_tokens, Some(10_000));
    assert_eq!(memory.close_input_tokens, Some(17_500));
    assert_eq!(memory.closed_memory_context_tokens, None);
    let memory_output_tokens = memory
        .memory_output_tokens
        .expect("memory output token count");
    assert_eq!(memory_output_tokens, 1_250);

    let captured = runtime
        .capture_closed_memory_context_accounting(1_250)
        .expect("capture closed memory accounting");
    assert!(captured);
    let accounting = runtime.store.mem_accounting().expect("memory accounting");
    assert_eq!(accounting.len(), 1);
    assert_eq!(accounting[0].closed_memory_context_tokens, 1_250);
    assert_eq!(accounting[0].provider_input_tokens, 1_250);
    assert_eq!(accounting[0].replacement_prefix_baseline_tokens, 0);

    let tree = runtime.render_tree().expect("render tree");
    assert!(tree.contains("[1.1.1] Done accounted child"), "{tree}");
    assert!(
        tree.contains("(~7.50K source -> ~1.25K memory context)"),
        "{tree}"
    );
    let materialized_before_snapshot = runtime
        .materialize_history_for_test(&raw)
        .expect("materialize before snapshot");
    let snapshot = runtime.build_tree_snapshot().expect("snapshot");
    assert_eq!(
        runtime
            .materialize_history_for_test(&raw)
            .expect("materialize after snapshot"),
        materialized_before_snapshot,
        "tree snapshot accounting must remain projection-only and not change h(PS)"
    );
    let snapshot_nodes = snapshot_nodes_by_id(&snapshot);
    assert_eq!(
        snapshot_nodes["1.1.1"].accounting,
        Some(SpineTreeNodeAccountingSnapshot {
            current_node_context_tokens: None,
            current_node_context_problem: None,
            current_node_context_baseline_source: None,
            closed_source_suffix_tokens: Some(7_500),
            closed_memory_context_tokens: Some(1_250),
            memory_output_tokens: Some(1_250),
        })
    );

    let replayed = SpineRuntime::load_for_rollout(&rollout, runtime.raw_len)
        .expect("load spine")
        .expect("sidecar exists");
    let replayed_tree = replayed.render_tree().expect("render replayed tree");
    assert!(
        replayed_tree.contains("(~7.50K source -> ~1.25K memory context)"),
        "{replayed_tree}"
    );
    let replayed_snapshot = replayed.build_tree_snapshot().expect("replay snapshot");
    let replayed_nodes = snapshot_nodes_by_id(&replayed_snapshot);
    assert_eq!(
        replayed_nodes["1.1.1"].accounting,
        snapshot_nodes["1.1.1"].accounting
    );
    let materialized = replayed.materialize_history_for_test(&raw).expect("materialize");
    assert_eq!(materialized.len(), 3);
    assert!(matches!(
        &materialized[0],
        ResponseItem::Message { content, .. }
            if matches!(
                content.as_slice(),
                [ContentItem::InputText { text }]
                    if text.contains("Spine Memory 1.1.1")
                        && text.contains("real compact body for 1.1.1")
            )
    ));
    assert_eq!(materialized[1], spine_call(SPINE_TOOL_CLOSE, "close"));
    assert_eq!(materialized[2], function_output("close"));
}
