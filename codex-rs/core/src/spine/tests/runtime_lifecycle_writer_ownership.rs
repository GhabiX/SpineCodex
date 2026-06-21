use super::*;

#[test]
#[serial(spine_writer_lock)]
fn installing_replayed_runtime_requires_sidecar_writer_ownership() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create first live spine");
    let replayed = SpineRuntime::load_for_rollout(&rollout, runtime.raw_len)
        .expect("read-only replay must not need writer ownership")
        .expect("sidecar exists");
    let mut state = SpineSessionState::new();

    let err = state
        .set_replayed(runtime.raw_len, Some(replayed))
        .expect_err("installing replay as a live runtime must require writer ownership");
    assert!(
        err.to_string()
            .contains("already owned by another live Codex process"),
        "unexpected writer lock error: {err}"
    );

    drop(runtime);
    eventually_set_replayed_writer(&mut state, &rollout, 0);
}
