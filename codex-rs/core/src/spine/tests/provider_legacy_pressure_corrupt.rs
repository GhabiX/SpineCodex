use super::*;

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
