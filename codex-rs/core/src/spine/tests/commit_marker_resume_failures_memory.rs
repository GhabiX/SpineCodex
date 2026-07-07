use super::*;

#[test]
fn resume_rejects_missing_memory_artifact() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    append_msg(
        &mut runtime,
        &mut raw,
        "root child work before missing memory",
    );
    close_task(&mut runtime, &mut raw, "close-missing-memory", "1.1");

    let marker = runtime
        .store
        .commit_markers()
        .expect("read commit markers")
        .into_iter()
        .next()
        .expect("close marker should exist");
    let memory = marker
        .memory_refs
        .first()
        .expect("close marker should reference memory");
    std::fs::remove_file(runtime.store.root.join(&memory.body_path))
        .expect("remove committed memory body");

    SpineRuntime::load_for_rollout_items(&rollout, &raw, &[])
        .expect_err("missing committed memory artifact must fail closed");
}
