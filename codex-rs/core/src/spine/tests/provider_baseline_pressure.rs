use super::provider_baseline::closed_child_accounting_with_source_suffix;
use super::provider_baseline::provider_token_baselines;
use super::*;

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
        provider_token_baselines(5_500),
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
        provider_token_baselines(9_500),
    );
    let snapshot = runtime.build_tree_snapshot().expect("snapshot");
    let nodes = snapshot_nodes_by_id(&snapshot);
    assert_eq!(
        nodes["1.1.1"].accounting,
        closed_child_accounting_with_source_suffix(4_000)
    );
}
