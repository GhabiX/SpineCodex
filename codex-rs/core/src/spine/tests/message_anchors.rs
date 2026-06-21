use super::*;

#[test]
fn ordinary_response_item_shifts_msg() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let item = text_item("ordinary");
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(1).expect("observe raw");
    runtime
        .observe_context_item(0, 0, &item)
        .expect("observe context item");

    let events = event_log(&runtime);
    assert!(matches!(
        events.as_slice(),
        [
            SpineLedgerEvent::Init { raw_start: 0 },
            SpineLedgerEvent::Open { summary, .. },
            SpineLedgerEvent::Msg {
                raw_ordinal: 0,
                context_index: 0,
                from_user: true,
                user_anchor: Some(1),
            }
        ] if summary == "root"
    ));
    assert_eq!(
        runtime.parse_stack().symbols,
        vec![
            Symbol::Control(ControlSymbol::Init(
                tree_meta(
                    &runtime.archive(),
                    NodeId::root_epoch(1),
                    0,
                    "root".to_string()
                )
                .expect("root meta")
            )),
            Symbol::Control(ControlSymbol::Open(
                tree_meta(
                    &runtime.archive(),
                    NodeId::root_epoch(1).child(1),
                    0,
                    "root".to_string()
                )
                .expect("root open meta")
            )),
            Symbol::SpineTreeNodes(vec![SpineTreeNode::MsgAsLeafNode {
                msg: SegRef::ResponseItem {
                    raw_ordinal: 0,
                    context_index: 0,
                },
                from_user: true,
                user_anchor: Some(1),
            }]),
        ]
    );
    let raw = vec![Some(item)];
    let materialized = runtime.materialize_history(&raw).expect("materialize");
    assert!(matches!(
        materialized.as_slice(),
        [ResponseItem::Message { content, .. }]
            if matches!(
                content.as_slice(),
                [ContentItem::InputText { text }] if text == "[U1]\nordinary"
            )
    ));
}

#[test]
fn non_user_message_does_not_receive_user_anchor() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let item = assistant_text_item("assistant note");
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(1).expect("observe raw");
    runtime
        .observe_context_item(0, 0, &item)
        .expect("observe context item");

    let events = event_log(&runtime);
    assert!(matches!(
        events.as_slice(),
        [
            SpineLedgerEvent::Init { .. },
            SpineLedgerEvent::Open { .. },
            SpineLedgerEvent::Msg {
                raw_ordinal: 0,
                context_index: 0,
                from_user: false,
                user_anchor: None,
            }
        ]
    ));
    let raw = vec![Some(item)];
    let materialized = runtime.materialize_history(&raw).expect("materialize");
    assert!(matches!(
        materialized.as_slice(),
        [ResponseItem::Message { content, .. }]
            if matches!(
                content.as_slice(),
                [ContentItem::OutputText { text }] if text == "assistant note"
            )
    ));
}

#[test]
fn close_memory_rejects_unknown_user_anchor_reference() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "known user");
    observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_CLOSE, "close");
    let err = runtime
        .stage_close(
            "close".to_string(),
            "This memory cites [U99], which does not exist.".to_string(),
        )
        .expect_err("unknown user anchor must fail");
    assert!(
        err.to_string().contains("unknown user anchor [U99]"),
        "{err}"
    );
    runtime
        .stage_close(
            "close".to_string(),
            "This memory cites the existing [U1].".to_string(),
        )
        .expect("known user anchor should be accepted");
}
