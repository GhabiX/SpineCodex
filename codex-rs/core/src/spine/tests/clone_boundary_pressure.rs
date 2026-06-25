use super::*;

#[test]
fn clone_boundary_excludes_future_structural_and_pressure_records() {
    assert_clone_boundary_excludes_future_structural_and_pressure_records();
}

pub(super) fn assert_clone_boundary_excludes_future_structural_and_pressure_records() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source_rollout = dir.path().join("source.jsonl");
    let target_rollout = dir.path().join("target.jsonl");
    let source = SpineStore::create_for_rollout(&source_rollout).expect("create source store");
    source
        .append_event(&SpineLedgerEvent::Init { raw_start: 0 })
        .expect("append init");
    source
        .append_event(&root_child_open_event("root"))
        .expect("append root open");
    source
        .append_event(&user_msg_event(0, 0))
        .expect("append kept msg");
    source
        .append_pressure_event(&pressure_baseline_event(3, 7_000, 7_500))
        .expect("append checkpoint-visible pressure");
    let boundary = SpineCloneBoundary {
        source_rollout_path: source_rollout,
        raw_ordinal_limit: 1,
        structural_seq_limit: source.next_event_seq().expect("structural seq limit"),
        pressure_seq_watermark: source
            .next_pressure_seq()
            .expect("pressure seq limit")
            .checked_sub(1),
        trim_seq_watermark: source
            .next_trim_seq()
            .expect("trim seq limit")
            .checked_sub(1),
        trim_toolcall_seq_limit: source.next_event_seq().expect("structural seq limit"),
    };

    source
        .append_event(&user_msg_event(0, 0))
        .expect("append future structural event");
    source
        .append_pressure_event(&pressure_baseline_event(4, 11_000, 11_500))
        .expect("append future pressure");

    SpineStore::clone_for_rollout_with_raw_live(&boundary, &target_rollout, &[true])
        .expect("clone sidecar");
    let target = SpineStore::for_rollout(&target_rollout).expect("target store");
    assert_eq!(
        target
            .events()
            .expect("read target events")
            .iter()
            .map(|event| event.seq)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
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
    let replayed =
        SpineRuntime::load_for_rollout_items(&target_rollout, &[Some(text_item("kept"))], &[])
            .expect("load target")
            .expect("target sidecar exists");
    assert_eq!(replayed.current_open_provider_input_tokens(), None);
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
        observed_raw_ordinal: 1,
        observed_raw_live_hash: Some(hash_raw_live(&[true])),
        observed_context_index: 1,
        context_tokens,
        input_tokens: Some(input_tokens),
        source: ContextBaselineSource::EstimatedFromLiveSuffix,
        estimated_live_suffix_tokens: Some(500),
    }
}
