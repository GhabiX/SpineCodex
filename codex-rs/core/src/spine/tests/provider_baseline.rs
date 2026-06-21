use super::*;

#[test]
fn provider_input_baseline_capture_records_structural_event() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "live suffix for root");
    let structural_seq_after_msg = runtime
        .build_tree_snapshot()
        .expect("snapshot")
        .snapshot_seq;
    let captured = runtime
        .capture_current_open_provider_baseline(9_000)
        .expect("capture provider baseline");
    assert!(captured);

    let snapshot = runtime.build_tree_snapshot().expect("snapshot");
    assert_eq!(snapshot.snapshot_seq, structural_seq_after_msg + 1);
    assert_eq!(runtime.store.event_count_for_test().expect("events"), 4);
    assert_eq!(runtime.store.pressure_events().expect("pressure").len(), 0);
    assert_eq!(runtime.current_open_input_tokens(), Some(9_000));
    assert_eq!(runtime.current_open_provider_input_tokens(), Some(9_000));
    assert_eq!(
        runtime.current_open_context_baseline_source(),
        Some(SpineNodeContextBaselineSource::ProviderAtOpen)
    );
}

#[test]
fn provider_input_baseline_replays_after_reload() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "live suffix");
    runtime
        .capture_current_open_provider_baseline(12_000)
        .expect("capture provider baseline");

    let replayed = SpineRuntime::load_for_rollout(&rollout, runtime.raw_len)
        .expect("load spine")
        .expect("sidecar exists");
    assert_eq!(replayed.current_open_input_tokens(), Some(12_000));
    assert_eq!(replayed.current_open_provider_input_tokens(), Some(12_000));
    assert_eq!(
        replayed.current_open_context_baseline_source(),
        Some(SpineNodeContextBaselineSource::ProviderAtOpen)
    );
}

#[test]
fn mismatched_legacy_open_baseline_encoding_fails_replay() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let store = SpineStore::create_for_rollout(&rollout).expect("create store");
    store
        .append_event(&SpineLedgerEvent::Init { raw_start: 0 })
        .expect("append init");
    store
        .append_event(&SpineLedgerEvent::Open {
            child: NodeId::root_epoch(1).child(1),
            boundary: 0,
            index: 0,
            summary: "root".to_string(),
            open_input_tokens: Some(12_345),
            open_context_tokens: Some(10_000),
            open_context_source: Some(ContextBaselineSource::ProviderAtOpen),
        })
        .expect("append mismatched open");

    let err = SpineRuntime::load_for_rollout(&rollout, 0)
        .expect_err("mismatched provider input encoding must fail closed");
    assert!(
        err.to_string().contains("mismatched provider input"),
        "unexpected error: {err}"
    );
}

#[test]
fn legacy_pressure_ledger_does_not_drive_live_baseline() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "live suffix");
    std::fs::write(
        runtime.store.pressure_path_for_test(),
        [
            format!(
                r#"{{"pressure_seq":0,"type":"open_context_baseline","node":[1,1],"observed_structural_seq":{},"observed_raw_ordinal":{},"observed_raw_live_hash":"{}","observed_context_index":{},"context_tokens":7000,"input_tokens":7500,"source":"estimated_from_live_suffix","estimated_live_suffix_tokens":500}}"#,
                runtime.store.next_event_seq().expect("next structural seq"),
                raw.len(),
                hash_raw_live(&vec![true; raw.len()]),
                raw.len()
            ),
            String::new(),
        ]
        .join("\n"),
    )
    .expect("write legacy pressure ledger");

    let replayed = SpineRuntime::load_for_rollout(&rollout, runtime.raw_len)
        .expect("load spine")
        .expect("sidecar exists");
    assert_eq!(replayed.current_open_input_tokens(), None);
    assert_eq!(replayed.current_open_provider_input_tokens(), None);
    assert_eq!(replayed.current_open_context_baseline_source(), None);
}

#[test]
fn closed_child_tree_snapshot_preserves_zero_source_suffix_accounting() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    open_task_with_token_baselines(
        &mut runtime,
        &mut raw,
        "open-zero-child",
        "zero child",
        SpineTokenBaselines {
            provider_input_tokens: Some(5_000),
        },
    );
    append_msg(&mut runtime, &mut raw, "zero child work");
    close_task_with_token_baselines(
        &mut runtime,
        &mut raw,
        "close-zero-child",
        "1.1.1",
        SpineTokenBaselines {
            provider_input_tokens: Some(5_000),
        },
    );

    let Some(Symbol::SpineTreeNodes(nodes)) = runtime.parse_stack().symbols.last() else {
        panic!("closed child should reduce into ParseStack nodes")
    };
    let memory = nodes
        .iter()
        .find_map(|node| match node {
            SpineTreeNode::SpineTree { memory, .. } => Some(memory),
            _ => None,
        })
        .expect("closed child memory ref");
    assert_eq!(memory.open_context_tokens, Some(5_000));
    assert_eq!(memory.close_context_tokens, Some(5_000));
    assert_eq!(memory.closed_source_suffix_tokens, Some(0));

    let snapshot = runtime.build_tree_snapshot().expect("snapshot");
    let nodes = snapshot_nodes_by_id(&snapshot);
    assert_eq!(
        nodes["1.1.1"].accounting,
        Some(SpineTreeNodeAccountingSnapshot {
            current_node_context_tokens: None,
            current_node_context_problem: None,
            current_node_context_baseline_source: None,
            closed_source_suffix_tokens: Some(0),
            closed_memory_context_tokens: None,
            memory_output_tokens: Some(1_250),
        })
    );

    let replayed = SpineRuntime::load_for_rollout(&rollout, runtime.raw_len)
        .expect("load spine")
        .expect("sidecar exists");
    let replayed_snapshot = replayed.build_tree_snapshot().expect("replay snapshot");
    let replayed_nodes = snapshot_nodes_by_id(&replayed_snapshot);
    assert_eq!(
        replayed_nodes["1.1.1"].accounting,
        nodes["1.1.1"].accounting
    );
}

#[test]
fn close_prefers_structural_open_baseline_over_pressure_overlay() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    open_task_with_token_baselines(
        &mut runtime,
        &mut raw,
        "open-structural-child",
        "structural child",
        SpineTokenBaselines {
            provider_input_tokens: Some(5_500),
        },
    );
    append_msg(&mut runtime, &mut raw, "child work");
    std::fs::write(
        runtime.store.pressure_path_for_test(),
        [
            format!(
                r#"{{"pressure_seq":0,"type":"open_context_baseline","node":[1,1,1],"observed_structural_seq":{},"observed_raw_ordinal":{},"observed_raw_live_hash":"{}","observed_context_index":{},"context_tokens":7000,"input_tokens":7500,"source":"estimated_from_live_suffix","estimated_live_suffix_tokens":500}}"#,
                runtime.store.next_event_seq().expect("next structural seq"),
                raw.len(),
                hash_raw_live(&vec![true; raw.len()]),
                raw.len()
            ),
            String::new(),
        ]
        .join("\n"),
    )
    .expect("write pressure overlay");
    let replayed = SpineRuntime::load_for_rollout(&rollout, runtime.raw_len)
        .expect("load spine")
        .expect("sidecar exists");
    assert_eq!(replayed.current_open_provider_input_tokens(), Some(5_500));
    assert_eq!(
        replayed.current_open_context_baseline_source(),
        Some(SpineNodeContextBaselineSource::ProviderAtOpen)
    );

    close_task_with_token_baselines(
        &mut runtime,
        &mut raw,
        "close-structural-child",
        "1.1.1",
        SpineTokenBaselines {
            provider_input_tokens: Some(9_500),
        },
    );
    let snapshot = runtime.build_tree_snapshot().expect("snapshot");
    let nodes = snapshot_nodes_by_id(&snapshot);
    assert_eq!(
        nodes["1.1.1"].accounting,
        Some(SpineTreeNodeAccountingSnapshot {
            current_node_context_tokens: None,
            current_node_context_problem: None,
            current_node_context_baseline_source: None,
            closed_source_suffix_tokens: Some(4_000),
            closed_memory_context_tokens: None,
            memory_output_tokens: Some(1_250),
        })
    );
}

#[test]
fn corrupt_legacy_pressure_records_do_not_fail_structural_replay() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    append_msg(&mut runtime, &mut raw, "live suffix");
    std::fs::write(
        runtime.store.pressure_path_for_test(),
        "not-json\n{\"pressure_seq\":77,\"type\":\"open_context_baseline\"",
    )
    .expect("corrupt pressure ledger");

    let replayed = SpineRuntime::load_for_rollout(&rollout, runtime.raw_len)
        .expect("load spine despite malformed pressure")
        .expect("sidecar exists");
    assert_eq!(replayed.current_open_provider_input_tokens(), None);
}
