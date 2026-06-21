use super::*;

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
