use super::*;

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
