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
        .append_event(&root_child_open_event("root"))
        .expect("append open");
    let raw_live = vec![true, false];
    let raw_live_hash = hash_raw_live(&raw_live);
    let body = "root compact after rollback hole";
    let (_, mem) = append_root_compact_memory_and_marker(
        &source,
        "root-1-2",
        body,
        0..2,
        raw_live_hash.clone(),
    );
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
