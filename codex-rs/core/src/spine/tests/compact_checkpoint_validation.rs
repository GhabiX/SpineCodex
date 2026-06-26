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
    let (body_path, mem) = append_default_root_compact_memory_and_marker(&source, "root-1-1", body);
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
