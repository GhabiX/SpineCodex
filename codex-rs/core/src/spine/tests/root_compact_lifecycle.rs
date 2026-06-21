use super::*;

// Native root compact and root epoch behavior.

#[test]
fn native_compact_shifts_compact_and_new_root_open() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw = vec![
        Some(text_item("before compact")),
        Some(text_item("more context")),
    ];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    runtime.observe_raw_items(2).expect("record raw");
    runtime
        .observe_context_item(0, 0, &text_item("before compact"))
        .expect("observe first context item");
    runtime
        .observe_context_item(1, 1, &text_item("more context"))
        .expect("observe second context item");

    runtime
        .root_compact("root summary".to_string(), &raw)
        .expect("compact root");

    let events = event_log(&runtime);
    assert!(matches!(
        events.as_slice(),
        [
            SpineLedgerEvent::Init { .. },
            SpineLedgerEvent::Open { summary, .. },
            SpineLedgerEvent::Msg { raw_ordinal: 0, .. },
            SpineLedgerEvent::Msg { raw_ordinal: 1, .. },
            SpineLedgerEvent::RootCompact {
                boundary: 2,
                next_open_index: 1,
                ..
            },
        ] if summary == "root"
    ));
    assert!(matches!(
        runtime.parse_stack().symbols.as_slice(),
        [
            Symbol::Control(ControlSymbol::Init(_)),
            Symbol::RootEpoches(root_epochs),
            Symbol::Control(ControlSymbol::Open(next_root)),
        ] if root_epochs.len() == 1
            && root_epochs[0].memory.node_id == NodeId::root_epoch(1)
            && root_epochs[0].memory.compact_id == "root-1-2"
            && next_root.id == NodeId::root_epoch(2).child(1)
            && next_root.index == 1
            && next_root.summary == "root"
    ));

    let replayed = SpineRuntime::load_for_rollout(&rollout, runtime.raw_len)
        .expect("load spine")
        .expect("sidecar exists");
    assert_eq!(
        replayed.parse_stack().symbols,
        runtime.parse_stack().symbols
    );
}
