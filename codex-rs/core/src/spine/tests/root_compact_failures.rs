use super::*;

// Clone and fork sidecar behavior.

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

#[test]
fn root_compact_new_root_accepts_post_compact_provider_baseline_capture() {
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
                close_input_tokens: Some(229_136),
                close_context_tokens: Some(230_871),
                next_open_input_tokens: None,
                next_open_context_tokens: None,
            },
        )
        .expect("compact root");
    assert_eq!(runtime.current_open_provider_input_tokens(), None);

    runtime
        .capture_current_open_provider_baseline(7_913)
        .expect("capture post-compact provider baseline");

    assert_eq!(runtime.current_open_input_tokens(), Some(7_913));
    assert_eq!(runtime.current_open_provider_input_tokens(), Some(7_913));
    assert_eq!(
        runtime.current_open_context_baseline_source(),
        Some(SpineNodeContextBaselineSource::ProviderAtOpen)
    );
    assert_ne!(runtime.current_open_provider_input_tokens(), Some(230_871));
}

#[test]
fn native_compact_failure_leaves_parse_stack_unchanged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    runtime.observe_raw_items(1).expect("record raw");
    runtime
        .observe_context_item(0, 0, &text_item("before failed compact"))
        .expect("observe context item");
    let parse_stack_before = runtime.parse_stack().clone();
    let tree_before = runtime.render_tree().expect("render tree before failure");
    let events_before = event_log_debug(&runtime);
    let mem_count_before = runtime
        .store
        .mems()
        .expect("read mems before failure")
        .len();

    let err = runtime
        .root_compact(
            "   \n\t".to_string(),
            &[Some(text_item("before failed compact"))],
        )
        .expect_err("empty native compact body must fail closed");
    assert!(
        err.to_string()
            .contains("spine root compact memory body must not be empty"),
        "unexpected empty compact error: {err}"
    );

    assert_parse_stack_tree_and_events_unchanged(
        &runtime,
        &parse_stack_before,
        &tree_before,
        &events_before,
    );
    assert_eq!(
        runtime.store.mems().expect("read mems after failure").len(),
        mem_count_before
    );
}

#[test]
fn root_compact_staging_failure_does_not_write_memory_body() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let init_meta = crate::spine::archive::tree_meta(
        &runtime.archive(),
        NodeId::root_epoch(1),
        0,
        "root".to_string(),
    )
    .expect("build init meta");
    let open_meta = crate::spine::archive::tree_meta(
        &runtime.archive(),
        NodeId::root_epoch(1).child(1),
        0,
        "root".to_string(),
    )
    .expect("build open meta");
    let close_memory = crate::spine::archive::memory_ref(
        &runtime.archive(),
        "invalid-close".to_string(),
        NodeId::root_epoch(1).child(1),
        crate::spine::io::sha1_hex("invalid close".as_bytes()),
        0..0,
        0..0,
        1..2,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    );
    runtime.parse_stack.symbols = vec![
        crate::spine::model::Symbol::Control(crate::spine::model::ControlSymbol::Init(init_meta)),
        crate::spine::model::Symbol::Control(crate::spine::model::ControlSymbol::Open(open_meta)),
        crate::spine::model::Symbol::Control(crate::spine::model::ControlSymbol::Close(
            close_memory,
        )),
    ];
    let parse_stack_before = runtime.parse_stack().clone();
    let events_before = event_log_debug(&runtime);
    let mem_count_before = runtime
        .store
        .mems()
        .expect("read mems before failure")
        .len();
    let compact_checkpoint_count_before = runtime
        .store
        .compact_checkpoints()
        .expect("read checkpoints before failure")
        .len();

    let err = runtime
        .root_compact_with_checkpoint(
            &rollout,
            "root summary after invalid close".to_string(),
            &[],
            SpineRootCompactTokenMetadata::default(),
        )
        .expect_err("invalid staged parse stack should fail before commit");
    assert!(
        err.to_string()
            .contains("spine.close requires non-empty live suffix"),
        "unexpected staging failure error: {err}"
    );

    assert_eq!(runtime.parse_stack(), &parse_stack_before);
    assert_eq!(event_log_debug(&runtime), events_before);
    assert_eq!(
        runtime.store.mems().expect("read mems after failure").len(),
        mem_count_before
    );
    assert_eq!(
        runtime
            .store
            .compact_checkpoints()
            .expect("read checkpoints after failure")
            .len(),
        compact_checkpoint_count_before
    );
    assert!(
        !runtime.store.root.join("memory/root-1-0.md").exists(),
        "root compact body must not be written before staging succeeds"
    );
}

#[test]
fn root_compact_survives_rollback_without_new_raw_items() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw_after_rollback = vec![Some(text_item("kept")), None];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    runtime.observe_raw_items(2).expect("record raw");
    runtime.raw_live = vec![true, false];
    runtime
        .root_compact(
            "root summary after rollback".to_string(),
            &raw_after_rollback,
        )
        .expect("compact root");

    let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw_after_rollback, &[])
        .expect("load spine")
        .expect("sidecar exists");
    let materialized = replayed
        .materialize_history(&raw_after_rollback)
        .expect("materialize");
    assert_eq!(materialized.len(), 1);
    assert!(matches!(
        &materialized[0],
        ResponseItem::Message { content, .. }
            if matches!(
                content.as_slice(),
                [ContentItem::InputText { text }]
                    if text.contains("root summary after rollback")
            )
    ));
}
