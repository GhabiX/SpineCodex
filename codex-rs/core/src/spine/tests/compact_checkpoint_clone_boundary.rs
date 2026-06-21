use super::*;

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
