use super::*;

#[test]
fn clone_preserves_pressure_seq_and_structural_refs() {
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
    source
        .append_pressure_event(&pressure_baseline_event(3, 7_000, 7_500))
        .expect("append pressure event");

    let raw_items = vec![None, Some(text_item("kept"))];
    clone_for_rollout_with_raw_live(&source_rollout, &target_rollout, &[false, true]);
    let target = SpineStore::for_rollout(&target_rollout).expect("target store");
    assert_eq!(
        target
            .events()
            .expect("read target events")
            .iter()
            .map(|event| event.seq)
            .collect::<Vec<_>>(),
        vec![0, 1, 3]
    );
    assert_eq!(
        target
            .pressure_events()
            .expect("read target pressure")
            .iter()
            .map(|event| event.pressure_seq)
            .collect::<Vec<_>>(),
        vec![0]
    );

    let replayed = SpineRuntime::load_for_rollout_items(&target_rollout, &raw_items, &[])
        .expect("load target")
        .expect("target sidecar exists");
    assert_eq!(replayed.current_open_provider_input_tokens(), None);

    let next_pressure_seq = target
        .append_pressure_event(&pressure_baseline_event(4, 8_000, 8_500))
        .expect("append pressure after clone");
    assert_eq!(next_pressure_seq, 1);
}

fn pressure_baseline_event(
    observed_structural_seq: u64,
    context_tokens: i64,
    input_tokens: i64,
) -> PressureEvent {
    PressureEvent::OpenContextBaseline {
        node: NodeId::root_epoch(1).child(1),
        open_structural_seq: Some(1),
        observed_structural_seq,
        observed_raw_ordinal: 2,
        observed_raw_live_hash: Some(hash_raw_live(&[false, true])),
        observed_context_index: 2,
        context_tokens,
        input_tokens: Some(input_tokens),
        source: ContextBaselineSource::EstimatedFromLiveSuffix,
        estimated_live_suffix_tokens: Some(500),
    }
}
