use super::*;

#[test]
fn abort_matching_pending_clears_control_call_without_durable_mutation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    append_msg(&mut runtime, &mut raw, "work before interrupted next");
    let request = spine_call(SPINE_TOOL_NEXT, "stale-next");
    let request_ordinal = u64::try_from(raw.len()).expect("raw ordinal fits u64");
    let request_context_index = current_context_len(&runtime, &raw);
    raw.push(Some(request.clone()));
    runtime.observe_raw_items(1).expect("record next request");
    runtime
        .observe_context_item(request_ordinal, request_context_index, &request)
        .expect("observe next request");
    runtime
        .stage_next(
            "stale-next".to_string(),
            "interrupted sibling".to_string(),
            "test node memory".to_string(),
        )
        .expect("stage next");

    let parse_stack_before = runtime.parse_stack().clone();
    let events_before = event_log_debug(&runtime);
    assert!(runtime.control_call_ids.contains("stale-next"));
    assert!(matches!(
        runtime
            .pending_commit("stale-next")
            .expect("pending commit"),
        Some(SpinePendingCommit::Close { .. })
    ));

    assert!(runtime.abort_pending("stale-next"));
    assert!(
        runtime
            .pending_commit("stale-next")
            .expect("pending should be cleared")
            .is_none()
    );
    assert!(!runtime.control_call_ids.contains("stale-next"));
    assert_eq!(runtime.parse_stack(), &parse_stack_before);
    assert_eq!(event_log_debug(&runtime), events_before);

    let next_request = spine_call(SPINE_TOOL_NEXT, "fresh-next");
    let next_ordinal = u64::try_from(raw.len()).expect("raw ordinal fits u64");
    let next_context_index = current_context_len(&runtime, &raw);
    raw.push(Some(next_request.clone()));
    runtime
        .observe_raw_items(1)
        .expect("record fresh next request");
    runtime
        .observe_context_item(next_ordinal, next_context_index, &next_request)
        .expect("observe fresh next request");
    runtime
        .stage_next(
            "fresh-next".to_string(),
            "fresh sibling".to_string(),
            "test node memory".to_string(),
        )
        .expect("fresh transition should stage after abort");
}

#[test]
fn abort_non_matching_pending_keeps_transition_until_stale_abort() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    append_msg(&mut runtime, &mut raw, "work before close");
    let request = spine_call(SPINE_TOOL_CLOSE, "close");
    let request_ordinal = u64::try_from(raw.len()).expect("raw ordinal fits u64");
    let request_context_index = current_context_len(&runtime, &raw);
    raw.push(Some(request.clone()));
    runtime.observe_raw_items(1).expect("record close request");
    runtime
        .observe_context_item(request_ordinal, request_context_index, &request)
        .expect("observe close request");
    runtime
        .stage_close("close".to_string(), "test node memory".to_string())
        .expect("stage close");

    assert!(!runtime.abort_pending("other-call"));
    assert!(runtime.control_call_ids.contains("close"));
    assert!(matches!(
        runtime.pending_commit("close").expect("pending close"),
        Some(SpinePendingCommit::Close { .. })
    ));

    assert_eq!(runtime.abort_any_pending().as_deref(), Some("close"));
    assert!(!runtime.control_call_ids.contains("close"));
    assert!(runtime.pending_commit("close").expect("cleared").is_none());
}
