use super::*;

#[test]
fn close_prepare_store_failure_retains_retryable_close_without_events() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    open_task(&mut runtime, &mut raw, "open-before-close-fail", "child");
    append_msg(&mut runtime, &mut raw, "child work before close failure");
    let (_request, request_raw, request_context) =
        observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_CLOSE, "close-store-fail");
    runtime
        .stage_close(
            "close-store-fail".to_string(),
            "memory that will fail before commit".to_string(),
        )
        .expect("stage close");
    let memory_assembly =
        close_memory_assembly_from_source_plan(&runtime, &raw, "close-store-fail", "1.1.1");
    let (_output, output_raw, output_context) =
        observe_function_output(&mut runtime, &mut raw, "close-store-fail");

    let before_events = ledger_event_debug(&runtime);
    let blocked_root = dir.path().join("not-a-dir-close");
    std::fs::write(&blocked_root, "file blocks sidecar dir").expect("write blocker file");
    runtime.store.root = blocked_root;

    runtime
        .prepare_commit_output_with_toolcall_and_raw_items(
            "close-store-fail",
            Some(memory_assembly),
            SpineTokenBaselines::default(),
            completed_toolcall(
                "close-store-fail",
                vec![
                    tool_segment(ToolCallSegmentKind::Request, request_raw, request_context),
                    tool_segment(ToolCallSegmentKind::Response, output_raw, output_context),
                ],
            ),
            &raw,
        )
        .expect_err("close prepare must fail while writing sidecar memory");
    assert_pending_close_retry_state(&runtime, &before_events);
}

#[test]
fn next_prepare_store_failure_retains_retryable_close_without_events() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    open_task(&mut runtime, &mut raw, "open-before-next-fail", "child");
    append_msg(&mut runtime, &mut raw, "child work before next failure");
    let (_request, request_raw, request_context) =
        observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_NEXT, "next-store-fail");
    runtime
        .stage_next(
            "next-store-fail".to_string(),
            "sibling that must not be installed".to_string(),
            "memory that will fail before next commit".to_string(),
        )
        .expect("stage next");
    let memory_assembly =
        close_memory_assembly_from_source_plan(&runtime, &raw, "next-store-fail", "1.1.1");
    let (_output, output_raw, output_context) =
        observe_function_output(&mut runtime, &mut raw, "next-store-fail");

    let before_events = ledger_event_debug(&runtime);
    let blocked_root = dir.path().join("not-a-dir-next");
    std::fs::write(&blocked_root, "file blocks sidecar dir").expect("write blocker file");
    runtime.store.root = blocked_root;

    runtime
        .prepare_commit_output_with_toolcall_and_raw_items(
            "next-store-fail",
            Some(memory_assembly),
            SpineTokenBaselines::default(),
            completed_toolcall(
                "next-store-fail",
                vec![
                    tool_segment(ToolCallSegmentKind::Request, request_raw, request_context),
                    tool_segment(ToolCallSegmentKind::Response, output_raw, output_context),
                ],
            ),
            &raw,
        )
        .expect_err("next prepare must fail while writing sidecar memory");
    assert_pending_close_retry_state(&runtime, &before_events);
}

#[test]
fn spine_error_classifies_missing_raw_coverage_as_sidecar_corruption() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let runtime = SpineRuntime::load_or_create(&rollout, 1).expect("create spine");
    let raw = vec![Some(text_item("uncovered durable item"))];

    let err = runtime
        .validate_raw_coverage(&raw)
        .expect_err("missing durable raw coverage must fail closed");
    assert_eq!(err.class(), SpineErrorClass::SidecarCorruption);
    assert!(err.should_invalidate_runtime());
    assert!(
        err.to_string()
            .contains("spine sidecar is missing token coverage for raw ordinal 0"),
        "unexpected coverage error: {err}"
    );
    assert!(err.to_string().contains("token_seq="));
}

#[test]
fn close_commit_marker_is_required_for_resume() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    append_msg(&mut runtime, &mut raw, "root child work before close");
    close_task(&mut runtime, &mut raw, "close-marker", "1.1");

    let markers = runtime
        .store
        .commit_markers_for_test()
        .expect("read commit markers");
    assert_eq!(markers.len(), 1);
    assert_eq!(markers[0].kind, SpineCommitKindMarker::Close);
    assert_eq!(markers[0].token_seq_end, markers[0].token_seq_start + 2);
    assert_eq!(markers[0].memory_refs.len(), 1);

    std::fs::remove_file(runtime.store.commit_path_for_test()).expect("remove commit markers");
    let err = SpineRuntime::load_for_rollout_items(&rollout, &raw, &[])
        .expect_err("Close ledger without commit marker must fail closed");
    assert!(
        err.to_string()
            .contains("missing Spine commit marker for Close ledger event"),
        "unexpected resume error: {err}"
    );
}

#[test]
fn next_commit_marker_covers_close_then_open_without_next_event() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    append_msg(&mut runtime, &mut raw, "root child work before next");
    next_task(&mut runtime, &mut raw, "next-marker", "1.1", "next sibling");

    let markers = runtime
        .store
        .commit_markers_for_test()
        .expect("read commit markers");
    assert_eq!(markers.len(), 1);
    assert_eq!(markers[0].kind, SpineCommitKindMarker::CloseThenOpen);
    assert_eq!(markers[0].token_seq_end, markers[0].token_seq_start + 3);
    let events = event_log(&runtime);
    assert!(matches!(
        events.as_slice(),
        [
            SpineLedgerEvent::Init { .. },
            SpineLedgerEvent::Open { .. },
            SpineLedgerEvent::Msg { .. },
            SpineLedgerEvent::Close { .. },
            SpineLedgerEvent::Open { .. },
            SpineLedgerEvent::ToolCall { .. },
        ]
    ));

    std::fs::remove_file(runtime.store.commit_path_for_test()).expect("remove commit markers");
    let err = SpineRuntime::load_for_rollout_items(&rollout, &raw, &[])
        .expect_err("Close+Open ledger without commit marker must fail closed");
    assert!(
        err.to_string()
            .contains("missing Spine commit marker for Close ledger event"),
        "unexpected resume error: {err}"
    );
}

#[test]
fn close_marker_does_not_replay_structural_close_without_live_toolcall_carrier() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    append_msg(&mut runtime, &mut raw, "root child work before close");
    close_task(&mut runtime, &mut raw, "close-carrier-live", "1.1");
    let full_history = runtime
        .materialize_history(&raw)
        .expect("materialize closed history");
    assert_eq!(full_history.len(), 3);

    let err = SpineRuntime::load_with_raw_live_and_event_limit(
        SpineStore::for_rollout(&rollout).expect("source store"),
        vec![true, false, false],
        None,
    )
    .expect_err("replay with stale close carrier raw must fail closed");
    assert!(
        err.to_string().contains("raw-backed event at token_seq"),
        "unexpected stale close carrier replay error: {err}"
    );
}

#[test]
fn commit_marker_replay_classifies_committed_and_uncommitted_proof() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    append_msg(
        &mut runtime,
        &mut raw,
        "root child work before replay classification",
    );
    close_task(&mut runtime, &mut raw, "close-replay-classification", "1.1");

    let marker = runtime
        .store
        .commit_markers_for_test()
        .expect("read close marker")
        .into_iter()
        .next()
        .expect("close marker should exist");
    let structural_event_seqs =
        commit_marker_structural_event_seqs(&marker).expect("marker structural seqs");
    let events = runtime
        .store
        .events()
        .expect("read events")
        .into_iter()
        .map(|event| (event.seq, event))
        .collect::<BTreeMap<_, _>>();
    let events_by_seq = events
        .iter()
        .map(|(seq, event)| (*seq, event))
        .collect::<BTreeMap<_, _>>();
    let mems = runtime
        .store
        .mems()
        .expect("read mem records")
        .into_iter()
        .map(|mem| (mem.compact_id.clone(), mem))
        .collect::<BTreeMap<_, _>>();
    let mems_by_id = mems
        .iter()
        .map(|(compact_id, mem)| (compact_id.as_str(), mem))
        .collect::<BTreeMap<_, _>>();

    assert_eq!(
        classify_commit_marker_for_replay(
            &marker,
            &structural_event_seqs,
            &events_by_seq,
            &mems_by_id,
            RawMask::new(&[true, true, true]),
            false,
        )
        .expect("classify committed marker"),
        ReplayCommitClassification::Committed
    );
    assert_eq!(
        classify_commit_marker_for_replay(
            &marker,
            &structural_event_seqs,
            &events_by_seq,
            &mems_by_id,
            RawMask::new(&[true, false, false]),
            false,
        )
        .expect("classify uncommitted marker"),
        ReplayCommitClassification::Uncommitted
    );
}

#[test]
fn clone_does_not_copy_marker_structural_close_without_live_toolcall_carrier() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source_rollout = dir.path().join("source.jsonl");
    let target_rollout = dir.path().join("target.jsonl");
    let mut runtime = SpineRuntime::load_or_create(&source_rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    append_msg(&mut runtime, &mut raw, "source work before close");
    close_task(&mut runtime, &mut raw, "close-not-cloned", "1.1");

    let boundary = SpineStore::clone_boundary_for_rollout(
        &source_rollout,
        u64::try_from(raw.len()).expect("raw len fits u64"),
    )
    .expect("capture clone boundary")
    .expect("source sidecar exists");
    let err = SpineStore::clone_for_rollout_with_raw_live(
        &boundary,
        &target_rollout,
        &[true, false, false],
    )
    .expect_err("clone sidecar without close carrier must fail closed");
    assert!(
        err.to_string().contains("clone raw live state"),
        "unexpected stale close carrier clone error: {err}"
    );
}

#[test]
fn root_compact_commit_marker_is_required_for_resume() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    append_msg(&mut runtime, &mut raw, "root visible work before compact");
    runtime
        .root_compact("root compact marker body".to_string(), &raw)
        .expect("root compact");

    let markers = runtime
        .store
        .commit_markers_for_test()
        .expect("read commit markers");
    assert_eq!(markers.len(), 1);
    assert_eq!(markers[0].kind, SpineCommitKindMarker::RootCompact);
    assert_eq!(markers[0].token_seq_end, markers[0].token_seq_start + 1);
    assert!(markers[0].raw_live_hash.is_some());

    std::fs::remove_file(runtime.store.commit_path_for_test()).expect("remove commit markers");
    let err = SpineRuntime::load_for_rollout_items(&rollout, &raw, &[])
        .expect_err("RootCompact ledger without commit marker must fail closed");
    assert!(
        err.to_string()
            .contains("missing Spine commit marker for RootCompact ledger event"),
        "unexpected resume error: {err}"
    );
}

#[test]
fn resume_ambiguous_partial_commit_fails_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    append_msg(
        &mut runtime,
        &mut raw,
        "root child work before ambiguous marker",
    );
    close_task(&mut runtime, &mut raw, "close-ambiguous-marker", "1.1");

    let mut duplicate = runtime
        .store
        .commit_markers_for_test()
        .expect("read commit markers")
        .into_iter()
        .next()
        .expect("close marker should exist");
    duplicate.op_id = "duplicate-close-marker".to_string();
    runtime
        .store
        .append_commit_marker(&duplicate)
        .expect("append duplicate marker");

    let err = SpineRuntime::load_for_rollout_items(&rollout, &raw, &[])
        .expect_err("ambiguous duplicate commit markers must fail closed");
    assert!(
        err.to_string()
            .contains("ambiguous Spine commit marker at token_seq"),
        "unexpected resume error: {err}"
    );
}

#[test]
fn resume_rejects_missing_memory_artifact() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    append_msg(
        &mut runtime,
        &mut raw,
        "root child work before missing memory",
    );
    close_task(&mut runtime, &mut raw, "close-missing-memory", "1.1");

    let marker = runtime
        .store
        .commit_markers_for_test()
        .expect("read commit markers")
        .into_iter()
        .next()
        .expect("close marker should exist");
    let memory = marker
        .memory_refs
        .first()
        .expect("close marker should reference memory");
    std::fs::remove_file(runtime.store.root.join(&memory.body_path))
        .expect("remove committed memory body");

    SpineRuntime::load_for_rollout_items(&rollout, &raw, &[])
        .expect_err("missing committed memory artifact must fail closed");
}
