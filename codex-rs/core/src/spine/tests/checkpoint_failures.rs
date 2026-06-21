use super::*;

#[test]
fn checkpoint_missing_required_field_fails_closed() {
    assert_checkpoint_missing_required_field_fails_closed();
}

#[test]
fn rollback_checkpoint_missing_field_fails_closed() {
    assert_checkpoint_missing_required_field_fails_closed();
}

fn assert_checkpoint_missing_required_field_fails_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw_after_rollback = vec![Some(text_item("kept")), None];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    runtime.observe_raw_items(1).expect("observe kept raw");
    runtime
        .observe_context_item(0, 0, &text_item("kept"))
        .expect("observe kept");
    let raw_before_rollback = vec![Some(text_item("kept"))];
    runtime
        .checkpoint_before_user_msg(&rollout, 1, &raw_before_rollback)
        .expect("write checkpoint");
    let checkpoint_path = runtime.store.checkpoint_path(1);
    let mut checkpoint = serde_json::to_value(
        runtime
            .store
            .checkpoint_for_test(1)
            .expect("read checkpoint"),
    )
    .expect("checkpoint to json value");
    checkpoint
        .as_object_mut()
        .expect("checkpoint object")
        .remove("parse_stack");
    std::fs::write(
        &checkpoint_path,
        serde_json::to_string_pretty(&checkpoint).expect("serialize checkpoint") + "\n",
    )
    .expect("overwrite checkpoint for missing field test");
    runtime
        .observe_raw_items(1)
        .expect("observe rolled-back raw");
    runtime
        .observe_context_item(1, 1, &text_item("rolled back"))
        .expect("observe rolled-back user");

    let err = SpineRuntime::load_for_rollout_items(&rollout, &raw_after_rollback, &[1])
        .expect_err("checkpoint with missing required field must fail closed");
    assert!(
        err.to_string().contains("missing field `parse_stack`"),
        "unexpected error: {err}"
    );
}

#[test]
fn corrupt_checkpoint_hash_fails_closed() {
    assert_corrupt_checkpoint_hash_fails_closed();
}

fn assert_corrupt_checkpoint_hash_fails_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw_after_rollback = vec![Some(text_item("kept")), None];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    runtime.observe_raw_items(1).expect("observe kept raw");
    runtime
        .observe_context_item(0, 0, &text_item("kept"))
        .expect("observe kept");
    let raw_before_rollback = vec![Some(text_item("kept"))];
    runtime
        .checkpoint_before_user_msg(&rollout, 1, &raw_before_rollback)
        .expect("write checkpoint");
    let checkpoint_path = runtime.store.checkpoint_path(1);
    let mut checkpoint = runtime
        .store
        .checkpoint_for_test(1)
        .expect("read checkpoint");
    checkpoint.h_ps_hash = "bad-hash".to_string();
    std::fs::write(
        &checkpoint_path,
        serde_json::to_string_pretty(&checkpoint).expect("serialize checkpoint") + "\n",
    )
    .expect("overwrite checkpoint for corruption test");
    runtime
        .observe_raw_items(1)
        .expect("observe rolled-back raw");
    runtime
        .observe_context_item(1, 1, &text_item("rolled back"))
        .expect("observe rolled-back user");

    let err = SpineRuntime::load_for_rollout_items(&rollout, &raw_after_rollback, &[1])
        .expect_err("corrupt checkpoint must fail closed");
    assert!(
        err.to_string()
            .contains("spine checkpoint h(PS) hash mismatch"),
        "unexpected error: {err}"
    );
}
