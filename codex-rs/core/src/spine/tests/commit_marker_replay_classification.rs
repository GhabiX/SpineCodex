use super::*;

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
        .commit_markers()
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
