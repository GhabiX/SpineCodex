use super::*;

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
