use super::*;

#[test]
fn rollback_without_pre_user_checkpoint_fails_closed() {
    assert_rollback_without_pre_user_checkpoint_fails_closed();
}

#[test]
fn rollback_does_not_parse_rendered_history() {
    assert_rollback_does_not_parse_rendered_history();
}

fn assert_rollback_without_pre_user_checkpoint_fails_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw_after_rollback = vec![Some(text_item("kept")), None, Some(text_item("new turn"))];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    runtime.observe_raw_items(3).expect("observe raw");
    runtime
        .observe_context_item(0, 0, &text_item("kept"))
        .expect("observe kept");
    runtime
        .observe_context_item(1, 1, &text_item("rolled back"))
        .expect("observe rolled-back user");
    runtime
        .observe_context_item(2, 1, &text_item("new turn"))
        .expect("observe new user");

    let err = SpineRuntime::load_for_rollout_items(&rollout, &raw_after_rollback, &[1])
        .expect_err("rollback without checkpoint must fail closed");
    assert!(
        err.to_string()
            .contains("missing spine rollback checkpoint before raw ordinal 1"),
        "unexpected error: {err}"
    );
}

fn assert_rollback_does_not_parse_rendered_history() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    append_msg(&mut runtime, &mut raw, "kept");
    runtime
        .checkpoint_before_user_msg(&rollout, 1, &raw)
        .expect("write rollback checkpoint");
    append_msg(&mut runtime, &mut raw, "rolled back");
    open_task(&mut runtime, &mut raw, "rendered-open", "rendered child");
    append_msg(&mut runtime, &mut raw, "rendered child work");
    close_task(&mut runtime, &mut raw, "rendered-close", "1.1.1");

    let rendered_history = runtime
        .materialize_history(&raw)
        .expect("materialize plausible rendered h(PS)");
    let rendered_memory = rendered_history
        .iter()
        .find(|item| {
            matches!(
                item,
                ResponseItem::Message { content, .. }
                    if matches!(
                        content.as_slice(),
                        [ContentItem::InputText { text }]
                            if text.contains("<spine_memory>")
                                && text.contains("Spine Memory 1.1.1")
                    )
            )
        })
        .cloned()
        .expect("rendered h(PS) should include plausible closed-child memory");
    let rendered_tree = runtime.render_tree().expect("render plausible tree");
    assert!(rendered_tree.contains("[1.1.1] Done rendered child"));

    std::fs::remove_file(runtime.store.checkpoint_path(1)).expect("remove rollback checkpoint");
    let raw_after_rollback = vec![
        Some(text_item("kept")),
        None,
        Some(rendered_memory),
        Some(text_item(&format!("Spine Task Tree:\n{rendered_tree}"))),
    ];

    let err = SpineRuntime::load_for_rollout_items(&rollout, &raw_after_rollback, &[1])
        .expect_err("rollback must fail closed instead of parsing rendered text");
    assert!(
        err.to_string()
            .contains("missing spine rollback checkpoint before raw ordinal 1"),
        "unexpected error: {err}"
    );
}
