use super::*;

#[test]
fn root_compact_separates_source_context_range_from_next_open_index() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw = vec![
        Some(text_item("root visible item 1")),
        Some(text_item("root visible item 2")),
        Some(text_item("root visible item 3")),
    ];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    runtime.observe_raw_items(raw.len()).expect("record raw");
    for (index, item) in raw.iter().enumerate() {
        runtime
            .observe_context_item(
                u64::try_from(index).expect("raw ordinal"),
                index,
                item.as_ref().expect("raw item"),
            )
            .expect("observe context item");
    }

    let before_len = runtime
        .materialize_variable_context_for_test(&raw)
        .expect("pre-compact h(PS)")
        .len();
    assert_eq!(before_len, 3);
    let materialized = runtime
        .root_compact("root compact summary".to_string(), &raw)
        .expect("compact root");
    assert_eq!(materialized.len(), 1);

    let events = event_log(&runtime);
    assert!(matches!(
        events.as_slice(),
        [
            SpineLedgerEvent::Init { .. },
            SpineLedgerEvent::Open { .. },
            SpineLedgerEvent::Msg { .. },
            SpineLedgerEvent::Msg { .. },
            SpineLedgerEvent::Msg { .. },
            SpineLedgerEvent::RootCompact {
                next_open_index: 1,
                ..
            },
        ]
    ));
    assert!(matches!(
        runtime.parse_stack().symbols.as_slice(),
        [
            Symbol::Control(ControlSymbol::Init(_)),
            Symbol::RootEpoches(root_epochs),
            Symbol::Control(ControlSymbol::Open(next_root)),
        ] if root_epochs.len() == 1
            && root_epochs[0].memory.source_context_range == (0..before_len)
            && next_root.index == materialized.len()
    ));
}
