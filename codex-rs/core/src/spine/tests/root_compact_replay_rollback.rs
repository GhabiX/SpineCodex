use super::*;

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
        .materialize_history_for_test(&raw_after_rollback)
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
