use super::*;

#[test]
fn ledger_cache_uses_sparse_max_seq_on_load_and_append() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let store = SpineStore::create_for_rollout(&rollout).expect("create store");
    store
        .append_logged_event(&LoggedSpineLedgerEvent {
            seq: 0,
            event: SpineLedgerEvent::Init { raw_start: 0 },
        })
        .expect("append sparse init");
    store
        .append_logged_event(&LoggedSpineLedgerEvent {
            seq: 7,
            event: SpineLedgerEvent::Open {
                child: NodeId::root_epoch(1).child(1),
                boundary: 0,
                index: 0,
                summary: "root".to_string(),
                open_input_tokens: None,
                open_context_tokens: None,
                open_context_source: None,
            },
        })
        .expect("append sparse root open");

    let mut runtime = SpineRuntime::load_for_rollout(&rollout, 0)
        .expect("load spine")
        .expect("sidecar exists");
    assert_eq!(runtime.ledger.next_event_seq, 8);
    assert_eq!(
        runtime
            .build_tree_snapshot()
            .expect("snapshot")
            .snapshot_seq,
        8
    );

    runtime.observe_raw_items(1).expect("observe raw");
    runtime
        .observe_context_item(0, 0, &text_item("after sparse ledger"))
        .expect("append msg");

    assert_eq!(runtime.ledger.next_event_seq, 9);
    let events = logged_events(&runtime);
    assert!(matches!(
        events.last(),
        Some(LoggedSpineLedgerEvent {
            seq: 8,
            event: SpineLedgerEvent::Msg { raw_ordinal: 0, .. }
        })
    ));
}
