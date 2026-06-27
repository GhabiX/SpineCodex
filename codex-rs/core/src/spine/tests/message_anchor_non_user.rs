use super::*;

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
    let materialized = runtime
        .materialize_history_for_test(&raw)
        .expect("materialize");
    assert!(matches!(
        materialized.as_slice(),
        [ResponseItem::Message { content, .. }]
            if matches!(
                content.as_slice(),
                [ContentItem::OutputText { text }] if text == "assistant note"
            )
    ));
}
