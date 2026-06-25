use super::*;

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
            .materialize_history_for_test(&raw_after_rollback)
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
