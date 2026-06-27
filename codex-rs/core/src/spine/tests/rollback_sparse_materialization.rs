use super::*;

#[test]
fn materialization_skips_rolled_back_raw_items_without_shifting_ordinals() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw = vec![
        Some(text_item("kept")),
        None,
        Some(text_item("after rollback")),
    ];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    runtime.observe_raw_items(3).expect("observe raw");
    runtime
        .observe_context_item(0, 0, &text_item("kept"))
        .expect("observe kept item");
    runtime
        .observe_context_item(2, 2, &text_item("after rollback"))
        .expect("observe surviving item");
    let materialized = runtime
        .materialize_history_for_test(&raw)
        .expect("materialize");

    assert_eq!(
        materialized,
        vec![
            anchored_text_item(1, "kept"),
            anchored_text_item(2, "after rollback")
        ]
    );
}
