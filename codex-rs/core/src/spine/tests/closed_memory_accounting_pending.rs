use super::*;

#[test]
fn closed_memory_context_accounting_missing_usage_consumes_pending() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    open_task_with_token_baselines(
        &mut runtime,
        &mut raw,
        "open-poc-missing-usage",
        "poc missing usage",
        SpineTokenBaselines {
            provider_input_tokens: Some(10_000),
        },
    );
    append_msg(&mut runtime, &mut raw, "inside");
    close_task_with_token_baselines(
        &mut runtime,
        &mut raw,
        "close-poc-missing-usage",
        "1.1.1",
        SpineTokenBaselines {
            provider_input_tokens: Some(17_500),
        },
    );

    assert!(
        runtime
            .store
            .mem_accounting()
            .expect("memory accounting")
            .is_empty(),
        "close should only stage pending accounting until a provider usage arrives"
    );

    let consumed = runtime
        .consume_closed_memory_context_accounting_without_provider_usage()
        .expect("missing provider usage consumes pending accounting");
    assert!(consumed);

    let captured = runtime
        .capture_closed_memory_context_accounting(2_500)
        .expect("later usage must not be accepted as first provider usage");
    assert!(
        !captured,
        "missing first provider usage must consume pending accounting"
    );
    let accounting = runtime.store.mem_accounting().expect("memory accounting");
    assert!(
        accounting.is_empty(),
        "missing provider usage must not fabricate a memory context size"
    );
}

#[test]
fn closed_memory_context_accounting_pending_survives_reload_before_first_usage() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    open_task_with_token_baselines(
        &mut runtime,
        &mut raw,
        "open-poc-reload",
        "poc reload",
        SpineTokenBaselines {
            provider_input_tokens: Some(10_000),
        },
    );
    append_msg(&mut runtime, &mut raw, "inside");
    close_task_with_token_baselines(
        &mut runtime,
        &mut raw,
        "close-poc-reload",
        "1.1.1",
        SpineTokenBaselines {
            provider_input_tokens: Some(17_500),
        },
    );
    assert!(
        runtime
            .store
            .mem_accounting()
            .expect("memory accounting")
            .is_empty(),
        "fixture should close memory before post-close provider usage"
    );
    let raw_len = runtime.raw_len;
    drop(runtime);

    let mut reloaded = eventually_load_or_create_writer(&rollout, raw_len);
    let captured = reloaded
        .capture_closed_memory_context_accounting(1_250)
        .expect("capture after reload should use durable pending accounting");
    assert!(
        captured,
        "pending memory accounting must be reconstructed from the sidecar"
    );
    let accounting = reloaded.store.mem_accounting().expect("memory accounting");
    assert_eq!(accounting.len(), 1);
    assert_eq!(accounting[0].closed_memory_context_tokens, 1_250);
}
