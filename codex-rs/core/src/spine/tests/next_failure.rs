use super::*;

#[test]
fn spine_next_close_failure_does_not_open_sibling() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "root child work");
    open_task(&mut runtime, &mut raw, "open-child", "nested child");
    append_msg(&mut runtime, &mut raw, "nested child work");

    let request = spine_call(SPINE_TOOL_NEXT, "bad-next");
    let request_ordinal = u64::try_from(raw.len()).expect("raw ordinal fits u64");
    let request_context_index = current_context_len(&runtime, &raw);
    raw.push(Some(request.clone()));
    runtime.observe_raw_items(1).expect("record next request");
    runtime
        .observe_context_item(request_ordinal, request_context_index, &request)
        .expect("observe next request");
    runtime
        .stage_next(
            "bad-next".to_string(),
            "next sibling".to_string(),
            "test node memory".to_string(),
        )
        .expect("stage next");
    let output = function_output("bad-next");
    runtime.observe_raw_items(1).expect("record next output");
    raw.push(Some(output.clone()));
    runtime
        .observe_context_item(5, 5, &output)
        .expect("observe next output");

    let err = runtime
        .maybe_commit_output(
            "bad-next",
            Some(memory_assembly_with_context_range("1.1.1", 0..raw.len())),
        )
        .expect_err("bad compact range should fail next");
    assert!(
        err.to_string().contains("expected suffix start 1"),
        "unexpected next failure: {err}"
    );
    assert!(matches!(
        runtime.parse_stack().symbols.as_slice(),
        [
            Symbol::Control(ControlSymbol::Init(_)),
            Symbol::Control(ControlSymbol::Open(root_child)),
            Symbol::SpineTreeNodes(_),
            Symbol::Control(ControlSymbol::Open(nested)),
            Symbol::SpineTreeNodes(_),
        ] if root_child.id == NodeId::root_epoch(1).child(1)
            && nested.id == NodeId::root_epoch(1).child(1).child(1)
    ));
    assert!(
        event_log(&runtime)
            .iter()
            .all(|event| !matches!(event, SpineLedgerEvent::Close { .. }))
    );
}
