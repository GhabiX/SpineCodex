use super::*;

#[test]
fn spine_open_lexer_emits_open_then_toolcall() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    let request = spine_call(SPINE_TOOL_OPEN, "open");
    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe open request");
    runtime
        .stage_open("open".to_string(), "child".to_string())
        .expect("stage open");
    let output = function_output("open");
    runtime.observe_raw_items(1).expect("record open output");
    runtime
        .observe_context_item(1, 1, &output)
        .expect("observe open output");
    runtime
        .maybe_commit_output("open", None)
        .expect("commit open");

    let events = event_log(&runtime);
    assert_eq!(events.len(), 4);
    assert!(matches!(events[0], SpineLedgerEvent::Init { raw_start: 0 }));
    assert!(matches!(
        &events[1],
        SpineLedgerEvent::Open {
            boundary: 0,
            summary,
            ..
        } if summary == "root"
    ));
    assert!(matches!(
        &events[2],
        SpineLedgerEvent::Open {
            boundary: 0,
            summary,
            ..
        } if summary == "child"
    ));
    assert!(matches!(
        &events[3],
        SpineLedgerEvent::ToolCall { segments }
            if segments == &vec![event_tool_req(0, 0), event_tool_resp(1, 1)]
    ));
    assert!(matches!(
        runtime.parse_stack().symbols.as_slice(),
        [
            Symbol::Control(ControlSymbol::Init(_)),
            Symbol::Control(ControlSymbol::Open(meta)),
            Symbol::Control(ControlSymbol::Open(child)),
            Symbol::SpineTreeNodes(nodes),
        ] if meta.summary == "root"
            && meta.id == NodeId::root_epoch(1).child(1)
            && child.summary == "child"
            && child.id == NodeId::root_epoch(1).child(1).child(1)
            && child.index == 0
            && matches!(
                nodes.as_slice(),
                [SpineTreeNode::ToolCallAsLeafNode { segments }]
                    if segments == &vec![tool_req(0, 0), tool_resp(1, 1)]
            )
    ));
    assert_eq!(
        runtime
            .materialize_history(&[Some(request.clone()), Some(output.clone())])
            .expect("materialize history"),
        vec![request, output]
    );
}

#[test]
fn duplicate_open_call_id_does_not_create_second_child() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    let request = spine_call(SPINE_TOOL_OPEN, "dup-open");
    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe first open request");

    runtime
        .observe_raw_items(1)
        .expect("record duplicate request");
    let err = runtime
        .observe_context_item(1, 1, &request)
        .expect_err("duplicate open request anchor must fail fast");
    assert!(
        err.to_string()
            .contains("duplicate spine.open request anchor for dup-open"),
        "unexpected duplicate error: {err}"
    );

    runtime
        .stage_open("dup-open".to_string(), "only child".to_string())
        .expect("stage open");
    let output = function_output("dup-open");
    runtime.observe_raw_items(1).expect("record open output");
    runtime
        .observe_context_item(2, 2, &output)
        .expect("observe open output");
    runtime
        .maybe_commit_output("dup-open", None)
        .expect("commit open");
    let events_after_first_commit = event_log(&runtime);
    let event_debug_after_first_commit = event_log_debug(&runtime);
    assert_eq!(
        events_after_first_commit
            .iter()
            .filter(
                |event| matches!(event, SpineLedgerEvent::Open { summary, .. } if summary == "only child")
            )
            .count(),
        1
    );
    assert_eq!(
        runtime
            .maybe_commit_output("dup-open", None)
            .expect("duplicate output commit should be no-op"),
        None
    );
    assert_eq!(event_log_debug(&runtime), event_debug_after_first_commit);
    let tree = runtime.render_tree().expect("render tree");
    assert!(tree.contains("Cursor: 1.1.1"), "{tree}");
    assert!(tree.contains("Spine Task Tree:"), "{tree}");
    assert!(tree.contains("- [1.1] Open"), "{tree}");
    assert!(tree.contains("- [1.1.1] Current only child"), "{tree}");
}

#[test]
fn open_append_failure_does_not_publish_parse_stack_or_cache() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let parse_stack_before = runtime.parse_stack().clone();
    let ledger_events_before = runtime
        .ledger
        .events
        .iter()
        .map(|event| format!("{event:?}"))
        .collect::<Vec<_>>();
    let next_event_seq_before = runtime.ledger.next_event_seq;

    let request = spine_call(SPINE_TOOL_OPEN, "open-fails");
    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe open request");
    runtime
        .stage_open("open-fails".to_string(), "unpublished child".to_string())
        .expect("stage open");

    let blocked_root = dir.path().join("not-a-dir");
    std::fs::write(&blocked_root, "file blocks sidecar dir").expect("write blocker file");
    runtime.store.root = blocked_root;

    runtime
        .maybe_commit_output("open-fails", None)
        .expect_err("open append should fail");
    assert_eq!(runtime.parse_stack(), &parse_stack_before);
    assert_eq!(
        runtime
            .ledger
            .events
            .iter()
            .map(|event| format!("{event:?}"))
            .collect::<Vec<_>>(),
        ledger_events_before
    );
    assert_eq!(runtime.ledger.next_event_seq, next_event_seq_before);
}
