use super::*;

#[test]
fn root_compact_checkpoint_append_failure_can_retry_without_duplicate_mem() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "root visible work");
    std::fs::create_dir_all(runtime.store.compact_checkpoint_path_for_test())
        .expect("block compact checkpoint append with directory");

    let err = runtime
        .root_compact_with_checkpoint(
            &rollout,
            "root compact summary".to_string(),
            &raw,
            SpineRootCompactTokenMetadata::default(),
        )
        .expect_err("blocked compact checkpoint append should fail");
    assert!(
        !err.to_string().is_empty(),
        "checkpoint append failure should surface"
    );
    assert!(
        !event_log(&runtime)
            .iter()
            .any(|event| matches!(event, SpineLedgerEvent::RootCompact { .. })),
        "failed checkpoint append must not commit RootCompact marker"
    );
    assert!(
        runtime
            .parse_stack()
            .symbols
            .iter()
            .any(|symbol| matches!(symbol, Symbol::Control(ControlSymbol::Compact(..)))),
        "failed root compact must retain the zero-width Compact token for retry"
    );
    let mems_after_failure = runtime.store.mems().expect("read mems after failure");
    assert_eq!(
        mems_after_failure.len(),
        1,
        "failed checkpoint append leaves exactly one prepared root mem"
    );

    std::fs::remove_dir_all(runtime.store.compact_checkpoint_path_for_test())
        .expect("unblock compact checkpoint append");
    let result = runtime
        .root_compact_with_checkpoint(
            &rollout,
            "root compact summary".to_string(),
            &raw,
            SpineRootCompactTokenMetadata::default(),
        )
        .expect("retry root compact after transient checkpoint failure");

    let mems_after_retry = runtime.store.mems().expect("read mems after retry");
    assert_eq!(
        mems_after_retry.len(),
        1,
        "retry must reuse matching root compact mem instead of appending duplicate"
    );
    runtime
        .store
        .validate_compact_checkpoint_for_boundary(
            &rollout,
            &runtime.raw_live,
            &raw,
            result.raw_boundary,
            &result.materialized,
        )
        .expect("retry checkpoint should validate against reused mem and RootCompact marker");
    assert!(!matches!(
        runtime.parse_stack().symbols.as_slice(),
        [.., Symbol::Control(ControlSymbol::Compact(..))]
    ));
}
