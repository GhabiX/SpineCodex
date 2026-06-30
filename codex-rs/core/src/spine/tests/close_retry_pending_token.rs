use super::*;

#[test]
fn close_retry_reduces_existing_pending_close_token() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    open_task(&mut runtime, &mut raw, "open", "child");
    append_msg(&mut runtime, &mut raw, "inside");
    observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_CLOSE, "close-retry");
    runtime
        .stage_close("close-retry".to_string(), "test node memory".to_string())
        .expect("stage close");
    let suffix_start = match runtime
        .pending_commit("close-retry")
        .expect("pending close")
    {
        Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
        other => panic!("expected pending close, got {other:?}"),
    };
    let close_request_index = current_context_len(&runtime, &raw) - 1;
    observe_function_output(&mut runtime, &mut raw, "close-retry");
    let memory_assembly =
        memory_assembly_with_context_range("1.1.1", suffix_start..close_request_index);

    let prepared_memory = runtime
        .prepared_close_memory_for_test(
            Some(memory_assembly.clone()),
            SpineTokenBaselines::default(),
        )
        .expect("prepare close commit");
    let archive = runtime.archive();
    runtime
        .parse_stack_mut_for_test()
        .shift_pending_close(prepared_memory, &archive)
        .expect("simulate retryable pending Close token");
    assert!(matches!(
        runtime.parse_stack().symbols.as_slice(),
        [
            ..,
            Symbol::Control(ControlSymbol::Open(_)),
            Symbol::SpineTreeNodes(_),
            Symbol::Control(ControlSymbol::Close(_))
        ]
    ));

    let commit = runtime
        .maybe_commit_output("close-retry", Some(memory_assembly))
        .expect("retry close")
        .expect("close should commit on retry");
    assert!(matches!(commit, SpineCommitKind::Close { .. }));
    assert!(!matches!(
        runtime.parse_stack().symbols.as_slice(),
        [
            ..,
            Symbol::Control(ControlSymbol::Open(_)),
            Symbol::SpineTreeNodes(_),
            Symbol::Control(ControlSymbol::Close(_))
        ]
    ));
    assert_eq!(
        runtime
            .store
            .commit_markers()
            .expect("commit markers")
            .len(),
        1
    );
}
