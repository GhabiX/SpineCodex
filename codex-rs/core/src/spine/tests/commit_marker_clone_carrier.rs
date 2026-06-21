use super::*;

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
