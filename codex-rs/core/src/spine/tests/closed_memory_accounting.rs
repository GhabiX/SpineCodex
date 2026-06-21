use super::*;

#[test]
fn closed_memory_context_accounting_rejects_invalid_first_provider_usage() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    open_task_with_token_baselines(
        &mut runtime,
        &mut raw,
        "open-accounted-child",
        "accounted child",
        SpineTokenBaselines {
            provider_input_tokens: Some(10_000),
        },
    );
    append_msg(&mut runtime, &mut raw, "inside");
    close_task_with_token_baselines(
        &mut runtime,
        &mut raw,
        "close-accounted-child",
        "1.1.1",
        SpineTokenBaselines {
            provider_input_tokens: Some(17_500),
        },
    );

    let captured = runtime
        .capture_closed_memory_context_accounting(17_500)
        .expect("invalid provider usage should not corrupt accounting");
    assert!(!captured);
    assert!(
        runtime
            .store
            .mem_accounting()
            .expect("memory accounting")
            .is_empty()
    );
    let tree = runtime.render_tree().expect("render tree");
    assert!(
        tree.contains("(~7.50K source -> ~1.25K memory output)"),
        "{tree}"
    );

    let second_capture = runtime
        .capture_closed_memory_context_accounting(1_250)
        .expect("first provider usage decision is single-shot");
    assert!(!second_capture);
}
