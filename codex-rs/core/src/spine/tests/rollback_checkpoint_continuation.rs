use super::*;

#[test]
fn rollback_checkpoint_replays_new_live_append_after_cut() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw_after_rollback = vec![
        Some(text_item("kept")),
        None,
        Some(text_item("after rollback")),
    ];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    runtime.observe_raw_items(1).expect("observe kept raw");
    runtime
        .observe_context_item(0, 0, &text_item("kept"))
        .expect("observe kept");
    let raw_before_rollback = vec![Some(text_item("kept"))];
    runtime
        .checkpoint_before_user_msg(&rollout, 1, &raw_before_rollback)
        .expect("write checkpoint");
    runtime
        .observe_raw_items(1)
        .expect("observe rolled-back raw");
    runtime
        .observe_context_item(1, 1, &text_item("rolled back"))
        .expect("observe rolled-back user");
    runtime.observe_raw_items(1).expect("observe new raw");
    runtime
        .observe_context_item(2, 1, &text_item("after rollback"))
        .expect("observe new user");

    let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw_after_rollback, &[1])
        .expect("load spine")
        .expect("sidecar exists");

    assert_eq!(
        replayed
            .materialize_history(&raw_after_rollback)
            .expect("materialize"),
        vec![
            anchored_text_item(1, "kept"),
            anchored_text_item(3, "after rollback")
        ]
    );
    let Some(Symbol::SpineTreeNodes(nodes)) = replayed.parse_stack().symbols.last() else {
        panic!("expected root nodes after replay")
    };
    assert!(matches!(
        nodes.as_slice(),
        [
            SpineTreeNode::MsgAsLeafNode {
                msg: SegRef::ResponseItem {
                    raw_ordinal: 0,
                    context_index: 0,
                },
                ..
            },
            SpineTreeNode::MsgAsLeafNode {
                msg: SegRef::ResponseItem {
                    raw_ordinal: 2,
                    context_index: 1,
                },
                ..
            },
        ]
    ));
}

#[test]
fn rollback_checkpoint_rebuilds_cache_from_full_sidecar_before_new_append() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw_after_rollback = vec![Some(text_item("kept")), None];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    runtime.observe_raw_items(1).expect("observe kept raw");
    runtime
        .observe_context_item(0, 0, &text_item("kept"))
        .expect("observe kept");
    let raw_before_rollback = vec![Some(text_item("kept"))];
    runtime
        .checkpoint_before_user_msg(&rollout, 1, &raw_before_rollback)
        .expect("write checkpoint");
    runtime
        .observe_raw_items(1)
        .expect("observe rolled-back raw");
    runtime
        .observe_context_item(1, 1, &text_item("rolled back"))
        .expect("observe rolled-back user");
    let full_sidecar_next_seq = runtime.ledger.next_event_seq;

    let mut replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw_after_rollback, &[1])
        .expect("load spine")
        .expect("sidecar exists");
    assert_eq!(replayed.ledger.next_event_seq, full_sidecar_next_seq);
    assert_eq!(
        replayed
            .materialize_history(&raw_after_rollback)
            .expect("materialize before append"),
        vec![anchored_text_item(1, "kept")]
    );

    raw_after_rollback.push(Some(text_item("after rollback")));
    replayed.observe_raw_items(1).expect("observe new raw");
    replayed
        .observe_context_item(2, 1, &text_item("after rollback"))
        .expect("append new raw after rollback replay");

    assert_eq!(replayed.ledger.next_event_seq, full_sidecar_next_seq + 1);
    let events = logged_events(&replayed);
    assert!(matches!(
        events.last(),
        Some(LoggedSpineLedgerEvent {
            seq,
            event: SpineLedgerEvent::Msg { raw_ordinal: 2, .. },
        }) if *seq == full_sidecar_next_seq
    ));
}

#[test]
fn rollback_checkpoint_new_open_reuses_restored_sibling_id() {
    assert_rollback_checkpoint_new_open_reuses_restored_sibling_id();
}

#[test]
fn rollback_allocates_correct_sibling_after_restore() {
    assert_rollback_checkpoint_new_open_reuses_restored_sibling_id();
}

fn assert_rollback_checkpoint_new_open_reuses_restored_sibling_id() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw_after_rollback = vec![
        Some(text_item("kept")),
        None,
        Some(spine_call(SPINE_TOOL_OPEN, "new-open")),
        Some(function_output("new-open")),
    ];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    runtime.observe_raw_items(1).expect("observe kept raw");
    runtime
        .observe_context_item(0, 0, &text_item("kept"))
        .expect("observe kept");
    let raw_before_rollback = vec![Some(text_item("kept"))];
    runtime
        .checkpoint_before_user_msg(&rollout, 1, &raw_before_rollback)
        .expect("write checkpoint");
    runtime
        .observe_raw_items(1)
        .expect("observe rolled-back raw");
    runtime
        .observe_context_item(1, 1, &text_item("rolled back"))
        .expect("observe rolled-back user");
    runtime
        .observe_raw_items(1)
        .expect("observe new open request");
    runtime
        .observe_context_item(2, 1, &spine_call(SPINE_TOOL_OPEN, "new-open"))
        .expect("observe new open request");
    runtime
        .stage_open("new-open".to_string(), "restored sibling".to_string())
        .expect("stage new open");
    runtime
        .observe_raw_items(1)
        .expect("observe new open output");
    runtime
        .observe_context_item(3, 2, &function_output("new-open"))
        .expect("observe new open output");
    runtime
        .maybe_commit_output("new-open", None)
        .expect("commit new open");

    let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw_after_rollback, &[1])
        .expect("load spine")
        .expect("sidecar exists");
    let tree = replayed.render_tree().expect("render tree");
    assert!(tree.contains("Cursor: 1.1.1"), "{tree}");
    assert!(tree.contains("- [1.1] Open"), "{tree}");
    assert!(
        tree.contains("- [1.1.1] Current restored sibling"),
        "{tree}"
    );
    assert!(matches!(
        replayed.parse_stack().symbols.as_slice(),
        [
            Symbol::Control(ControlSymbol::Init(_)),
            Symbol::Control(ControlSymbol::Open(root)),
            Symbol::SpineTreeNodes(nodes),
            Symbol::Control(ControlSymbol::Open(child)),
            Symbol::SpineTreeNodes(child_nodes),
        ] if root.id == NodeId::root_epoch(1).child(1)
            && matches!(
                nodes.as_slice(),
                [SpineTreeNode::MsgAsLeafNode {
                    msg: SegRef::ResponseItem {
                        raw_ordinal: 0,
                        context_index: 0,
                    },
                    ..
                }]
            )
            && child.id == NodeId::root_epoch(1).child(1).child(1)
            && child.index == 1
            && child.summary == "restored sibling"
            && matches!(
                child_nodes.as_slice(),
                [SpineTreeNode::ToolCallAsLeafNode { segments }]
                    if segments == &vec![tool_req(2, 1), tool_resp(3, 2)]
            )
    ));
}

#[test]
fn rollback_without_pre_user_checkpoint_fails_closed() {
    assert_rollback_without_pre_user_checkpoint_fails_closed();
}

#[test]
fn rollback_does_not_parse_rendered_history() {
    assert_rollback_does_not_parse_rendered_history();
}

fn assert_rollback_without_pre_user_checkpoint_fails_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw_after_rollback = vec![Some(text_item("kept")), None, Some(text_item("new turn"))];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    runtime.observe_raw_items(3).expect("observe raw");
    runtime
        .observe_context_item(0, 0, &text_item("kept"))
        .expect("observe kept");
    runtime
        .observe_context_item(1, 1, &text_item("rolled back"))
        .expect("observe rolled-back user");
    runtime
        .observe_context_item(2, 1, &text_item("new turn"))
        .expect("observe new user");

    let err = SpineRuntime::load_for_rollout_items(&rollout, &raw_after_rollback, &[1])
        .expect_err("rollback without checkpoint must fail closed");
    assert!(
        err.to_string()
            .contains("missing spine rollback checkpoint before raw ordinal 1"),
        "unexpected error: {err}"
    );
}

fn assert_rollback_does_not_parse_rendered_history() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    append_msg(&mut runtime, &mut raw, "kept");
    runtime
        .checkpoint_before_user_msg(&rollout, 1, &raw)
        .expect("write rollback checkpoint");
    append_msg(&mut runtime, &mut raw, "rolled back");
    open_task(&mut runtime, &mut raw, "rendered-open", "rendered child");
    append_msg(&mut runtime, &mut raw, "rendered child work");
    close_task(&mut runtime, &mut raw, "rendered-close", "1.1.1");

    let rendered_history = runtime
        .materialize_history(&raw)
        .expect("materialize plausible rendered h(PS)");
    let rendered_memory = rendered_history
        .iter()
        .find(|item| {
            matches!(
                item,
                ResponseItem::Message { content, .. }
                    if matches!(
                        content.as_slice(),
                        [ContentItem::InputText { text }]
                            if text.contains("<spine_memory>")
                                && text.contains("Spine Memory 1.1.1")
                    )
            )
        })
        .cloned()
        .expect("rendered h(PS) should include plausible closed-child memory");
    let rendered_tree = runtime.render_tree().expect("render plausible tree");
    assert!(rendered_tree.contains("[1.1.1] Done rendered child"));

    std::fs::remove_file(runtime.store.checkpoint_path(1)).expect("remove rollback checkpoint");
    let raw_after_rollback = vec![
        Some(text_item("kept")),
        None,
        Some(rendered_memory),
        Some(text_item(&format!("Spine Task Tree:\n{rendered_tree}"))),
    ];

    let err = SpineRuntime::load_for_rollout_items(&rollout, &raw_after_rollback, &[1])
        .expect_err("rollback must fail closed instead of parsing rendered text");
    assert!(
        err.to_string()
            .contains("missing spine rollback checkpoint before raw ordinal 1"),
        "unexpected error: {err}"
    );
}
