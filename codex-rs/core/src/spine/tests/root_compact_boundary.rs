use super::*;

#[test]
fn root_compact_from_root_cursor_after_closing_first_child_opens_next_epoch() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "root child work");
    close_task(&mut runtime, &mut raw, "close-1-1", "1.1");
    let pre_compact = runtime
        .materialize_history_for_test(&raw)
        .expect("materialize");

    let materialized = runtime
        .root_compact("root epoch summary after closing 1.1".to_string(), &raw)
        .expect("compact root cursor");
    assert_eq!(materialized.len(), 1);

    let events = event_log(&runtime);
    assert!(matches!(
        events.as_slice(),
        [
            SpineLedgerEvent::Init { .. },
            SpineLedgerEvent::Open { child: first_child, .. },
            SpineLedgerEvent::Msg { .. },
            SpineLedgerEvent::Close { node: closed_node, .. },
            SpineLedgerEvent::ToolCall { .. },
            SpineLedgerEvent::RootCompact {
                node: compacted_epoch,
                next_open_index: 1,
                ..
            },
        ] if *first_child == NodeId::root_epoch(1).child(1)
            && *closed_node == NodeId::root_epoch(1).child(1)
            && *compacted_epoch == NodeId::root_epoch(1)
    ));
    assert!(matches!(
        runtime.parse_stack().symbols.as_slice(),
        [
            Symbol::Control(ControlSymbol::Init(_)),
            Symbol::RootEpoches(root_epochs),
            Symbol::Control(ControlSymbol::Open(next_root)),
        ] if root_epochs.len() == 1
            && root_epochs[0].memory.node_id == NodeId::root_epoch(1)
            && root_epochs[0].memory.source_context_range == (0..pre_compact.len())
            && next_root.id == NodeId::root_epoch(2).child(1)
            && next_root.index == materialized.len()
    ));

    let replayed = SpineRuntime::load_for_rollout(&rollout, runtime.raw_len)
        .expect("reload spine")
        .expect("sidecar exists");
    assert_eq!(
        replayed.parse_stack().symbols,
        runtime.parse_stack().symbols
    );
    let snapshot = replayed.build_tree_snapshot().expect("snapshot");
    assert_snapshot_is_self_contained_forest(&snapshot);
    assert_eq!(snapshot.active_node_id, "2.1");
}
