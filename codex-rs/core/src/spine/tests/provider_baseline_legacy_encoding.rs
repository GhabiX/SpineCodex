use super::*;

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
