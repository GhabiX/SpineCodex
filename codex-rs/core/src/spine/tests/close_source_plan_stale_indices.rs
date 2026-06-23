use super::*;

#[test]
fn close_source_plan_ignores_stale_persisted_leaf_context_indices() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    open_task(&mut runtime, &mut raw, "open-stale", "stale index task");
    append_msg(&mut runtime, &mut raw, "first live item");
    append_msg(&mut runtime, &mut raw, "second live item");

    let Some(Symbol::SpineTreeNodes(nodes)) = runtime.parse_stack_mut_for_test().symbols.last_mut()
    else {
        panic!("open task should have live suffix nodes");
    };
    let [
        SpineTreeNode::ToolCallAsLeafNode { segments },
        SpineTreeNode::MsgAsLeafNode { msg: first_msg, .. },
        SpineTreeNode::MsgAsLeafNode {
            msg: second_msg, ..
        },
    ] = nodes.as_mut_slice()
    else {
        panic!("unexpected live suffix nodes: {nodes:?}");
    };
    for (offset, segment) in segments.iter_mut().enumerate() {
        let SegRef::ResponseItem { context_index, .. } = &mut segment.seg else {
            panic!("toolcall segment should be raw response item");
        };
        *context_index = 11 + offset;
    }
    let SegRef::ResponseItem {
        context_index: first_context_index,
        ..
    } = first_msg
    else {
        panic!("first msg should be raw response item");
    };
    *first_context_index = 9;
    let SegRef::ResponseItem {
        context_index: second_context_index,
        ..
    } = second_msg
    else {
        panic!("second msg should be raw response item");
    };
    *second_context_index = 10;

    observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_CLOSE, "close-stale");
    runtime
        .stage_close("close-stale".to_string(), "test node memory".to_string())
        .expect("stage close");
    let host_history = runtime
        .materialize_history(&raw)
        .expect("materialize current h(PS)");
    let source_plan = pending_close_source_plan(&runtime, &host_history, "close-stale", "1.1.1");
    let contexts = source_plan
        .entries
        .iter()
        .map(|entry| entry.context_index)
        .collect::<Vec<_>>();
    assert_eq!(contexts, vec![0, 1, 2, 3]);
    assert_eq!(source_plan.source_context_range, 0..4);
}
