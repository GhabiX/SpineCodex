use super::*;

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
