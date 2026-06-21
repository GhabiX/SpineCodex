use super::*;

#[test]
fn clone_for_rollout_keeps_compact_checkpoint_for_matching_raw_live_hash() {
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
        .expect("append open");
    let raw_live = vec![true, false];
    let raw_live_hash = hash_raw_live(&raw_live);
    let body = "root compact after rollback hole";
    let body_path = source
        .write_memory_body("root-1-2", body)
        .expect("write source body");
    let mem = MemRecord {
        compact_id: "root-1-2".to_string(),
        kind: MemKind::RootEpoch,
        node: NodeId::root_epoch(1),
        raw_start: 0,
        raw_end: 2,
        context_start: 0,
        context_end: 1,
        raw_live_hash: Some(raw_live_hash.clone()),
        open_input_tokens: None,
        close_input_tokens: None,
        open_context_tokens: None,
        close_context_tokens: None,
        closed_source_suffix_tokens: None,
        closed_memory_context_tokens: None,
        open_context_source: None,
        memory_output_tokens: None,
        body_path: body_path,
        body_hash: sha1_hex(body.as_bytes()),
    };
    source.append_mem(&mem).expect("append mem");
    source
        .append_event(&SpineLedgerEvent::RootCompact {
            node: NodeId::root_epoch(1),
            boundary: 2,
            mem: "root-1-2".to_string(),
            next_open_index: 1,
            raw_live_hash: raw_live_hash.clone(),
            next_open_input_tokens: None,
            next_open_context_tokens: None,
        })
        .expect("append root compact");
    source
        .append_compact_checkpoint(&root_compact_checkpoint_for_memory(
            &source_rollout,
            &mem,
            body,
            2,
            3,
            "memory/source-only.md".to_string(),
        ))
        .expect("append compact checkpoint");

    clone_for_rollout_with_raw_live(&source_rollout, &target_rollout, &raw_live);
    let target = SpineStore::for_rollout(&target_rollout).expect("target store");
    assert_eq!(
        target
            .compact_checkpoints()
            .expect("read target checkpoints")
            .len(),
        1
    );
    target
        .validate_compact_checkpoint_for_boundary(
            &target_rollout,
            &raw_live,
            &[],
            2,
            &[memory_response_item(body)],
        )
        .expect("rollback-hole checkpoint should validate against target sidecar");
}

#[test]
fn clone_boundary_excludes_future_compact_checkpoint_and_memory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source_rollout = dir.path().join("source.jsonl");
    let target_rollout = dir.path().join("target.jsonl");
    let mut raw = Vec::new();
    let mut source_runtime =
        SpineRuntime::load_or_create(&source_rollout, 0).expect("create source runtime");
    append_msg(&mut source_runtime, &mut raw, "boundary-visible");
    let boundary = SpineStore::clone_boundary_for_rollout(&source_rollout, 1)
        .expect("capture boundary")
        .expect("source sidecar exists");

    source_runtime
        .root_compact_with_checkpoint(
            &source_rollout,
            "future compact body".to_string(),
            &raw,
            SpineRootCompactTokenMetadata::default(),
        )
        .expect("future root compact");

    SpineStore::clone_for_rollout_with_raw_live(&boundary, &target_rollout, &[true])
        .expect("clone sidecar");
    let target = SpineStore::for_rollout(&target_rollout).expect("target store");
    assert!(
        target.mems().expect("read target mem records").is_empty(),
        "fork boundary must not clone future memory bodies"
    );
    assert!(
        target
            .compact_checkpoints()
            .expect("read target compact checkpoints")
            .is_empty(),
        "fork boundary must not clone future compact checkpoints"
    );
    assert_eq!(
        target
            .events()
            .expect("read target events")
            .iter()
            .map(|event| event.seq)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
}
