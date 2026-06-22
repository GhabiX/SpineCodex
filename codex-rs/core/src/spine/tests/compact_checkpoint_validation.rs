use super::*;

#[test]
fn clone_for_rollout_rewrites_compact_checkpoint_memory_refs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source_rollout = dir.path().join("source.jsonl");
    let target_rollout = dir.path().join("target.jsonl");
    let source = SpineStore::create_for_rollout(&source_rollout).expect("create source store");
    source
        .append_event(&SpineLedgerEvent::Init { raw_start: 0 })
        .expect("append init");
    let body = "root compact body";
    let body_path = source
        .write_memory_body("root-1-1", body)
        .expect("write source body");
    let mem = root_epoch_mem_record("root-1-1", body, body_path.clone());
    source.append_mem(&mem).expect("append mem");
    source
        .append_event(&SpineLedgerEvent::RootCompact {
            node: NodeId::root_epoch(1),
            boundary: 0,
            mem: "root-1-1".to_string(),
            next_open_index: 1,
            raw_live_hash: hash_raw_live(&[]),
            next_open_input_tokens: None,
            next_open_context_tokens: None,
        })
        .expect("append root compact");
    source
        .append_compact_checkpoint(&root_compact_checkpoint_for_memory(
            &source_rollout,
            &mem,
            body,
            1,
            2,
            "memory/source-only.md".to_string(),
        ))
        .expect("append compact checkpoint");

    clone_for_rollout_with_raw_live(&source_rollout, &target_rollout, &[]);
    let target = SpineStore::for_rollout(&target_rollout).expect("target store");
    let checkpoint = target
        .compact_checkpoints()
        .expect("read target checkpoints")
        .pop()
        .expect("target checkpoint");

    assert_eq!(
        checkpoint.rollout_path,
        target_rollout.display().to_string()
    );
    assert_eq!(checkpoint.memory_refs[0].body_path, body_path);
    target
        .validate_compact_checkpoint_for_boundary(
            &target_rollout,
            &[],
            &[],
            0,
            &[memory_response_item(body)],
        )
        .expect("cloned checkpoint should validate against target sidecar");
}

pub(super) fn root_epoch_mem_record(compact_id: &str, body: &str, body_path: String) -> MemRecord {
    MemRecord {
        compact_id: compact_id.to_string(),
        kind: MemKind::RootEpoch,
        node: NodeId::root_epoch(1),
        raw_start: 0,
        raw_end: 0,
        context_start: 0,
        context_end: 1,
        raw_live_hash: Some(hash_raw_live(&[])),
        open_input_tokens: None,
        close_input_tokens: None,
        open_context_tokens: None,
        close_context_tokens: None,
        closed_source_suffix_tokens: None,
        closed_memory_context_tokens: None,
        open_context_source: None,
        memory_output_tokens: None,
        body_path,
        body_hash: sha1_hex(body.as_bytes()),
    }
}
