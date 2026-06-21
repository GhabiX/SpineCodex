use super::*;

#[test]
fn clone_preserves_structural_seq_gaps_and_appends_after_max() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source_rollout = dir.path().join("source.jsonl");
    let target_rollout = dir.path().join("target.jsonl");
    let source = SpineStore::create_for_rollout(&source_rollout).expect("create source store");
    source
        .append_event(&SpineLedgerEvent::Init { raw_start: 0 })
        .expect("append init");
    source
        .append_event(&SpineLedgerEvent::Open {
            child: NodeId::root_epoch(1).child(1),
            boundary: 0,
            index: 0,
            summary: "root".to_string(),
            open_input_tokens: None,
            open_context_tokens: None,
            open_context_source: None,
        })
        .expect("append root open");
    source
        .append_event(&SpineLedgerEvent::Msg {
            raw_ordinal: 0,
            context_index: 0,
            from_user: true,
            user_anchor: None,
        })
        .expect("append dropped msg");
    source
        .append_event(&SpineLedgerEvent::Msg {
            raw_ordinal: 1,
            context_index: 1,
            from_user: true,
            user_anchor: None,
        })
        .expect("append kept msg");

    clone_for_rollout_with_raw_live(&source_rollout, &target_rollout, &[false, true]);
    let target = SpineStore::for_rollout(&target_rollout).expect("target store");
    let cloned_events = target.events().expect("read target events");
    assert_eq!(
        cloned_events
            .iter()
            .map(|event| event.seq)
            .collect::<Vec<_>>(),
        vec![0, 1, 3]
    );

    let next_seq = target
        .append_event(&SpineLedgerEvent::Msg {
            raw_ordinal: 2,
            context_index: 2,
            from_user: true,
            user_anchor: None,
        })
        .expect("append after gapped clone");
    assert_eq!(next_seq, 4);
}
