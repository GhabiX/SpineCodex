use super::*;

#[test]
fn root_compact_ignores_next_open_handoff_tokens() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "root visible work");
    runtime
        .root_compact_with_checkpoint(
            &rollout,
            "root compact summary".to_string(),
            &raw,
            SpineRootCompactTokenMetadata {
                close_input_tokens: Some(111_222),
                close_context_tokens: Some(222_333),
                next_open_input_tokens: Some(12_345),
                next_open_context_tokens: Some(67_890),
            },
        )
        .expect("compact root");

    assert_eq!(runtime.current_open_input_tokens(), None);
    assert_eq!(runtime.current_open_provider_input_tokens(), None);
    assert_eq!(runtime.current_open_context_baseline_source(), None);

    let events = event_log(&runtime);
    assert!(matches!(
        events.as_slice(),
        [
            SpineLedgerEvent::Init { .. },
            SpineLedgerEvent::Open { .. },
            SpineLedgerEvent::Msg { .. },
            SpineLedgerEvent::RootCompact {
                next_open_input_tokens: None,
                next_open_context_tokens: None,
                ..
            },
        ]
    ));

    let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw, &[])
        .expect("load spine")
        .expect("sidecar exists");
    assert_eq!(replayed.current_open_input_tokens(), None);
    assert_eq!(replayed.current_open_provider_input_tokens(), None);
    assert_eq!(replayed.current_open_context_baseline_source(), None);
}
