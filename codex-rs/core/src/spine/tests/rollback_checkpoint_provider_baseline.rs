use super::*;

#[test]
fn rollback_checkpoint_replays_checkpoint_visible_provider_baseline() {
    assert_rollback_checkpoint_replays_checkpoint_visible_provider_baseline();
}

#[test]
fn rollback_restores_checkpoint_visible_provider_baseline() {
    assert_rollback_checkpoint_replays_checkpoint_visible_provider_baseline();
}

fn assert_rollback_checkpoint_replays_checkpoint_visible_provider_baseline() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw_after_rollback = vec![Some(text_item("kept")), None];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    runtime.observe_raw_items(1).expect("observe kept raw");
    runtime
        .observe_context_item(0, 0, &text_item("kept"))
        .expect("observe kept");
    runtime
        .capture_current_open_provider_baseline(8_000)
        .expect("capture pre-checkpoint provider baseline");
    let raw_before_rollback = vec![Some(text_item("kept"))];
    runtime
        .checkpoint_before_user_msg(&rollout, 1, &raw_before_rollback)
        .expect("write checkpoint after provider baseline");
    runtime
        .observe_raw_items(1)
        .expect("observe rolled-back raw");
    runtime
        .observe_context_item(1, 1, &text_item("rolled back"))
        .expect("observe rolled-back user");

    let checkpoint = runtime
        .store
        .checkpoint_for_test(1)
        .expect("read checkpoint");
    assert_eq!(checkpoint.pressure_seq_watermark, None);

    let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw_after_rollback, &[1])
        .expect("load spine")
        .expect("sidecar exists");
    assert_eq!(replayed.current_open_provider_input_tokens(), Some(8_000));
}
