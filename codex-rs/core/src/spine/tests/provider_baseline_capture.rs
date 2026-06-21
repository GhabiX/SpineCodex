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
