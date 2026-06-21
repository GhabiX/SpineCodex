use super::*;

#[test]
#[serial(spine_writer_lock)]
fn second_live_runtime_for_same_sidecar_fails_fast() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create first live spine");

    let replayed = SpineRuntime::load_for_rollout(&rollout, runtime.raw_len)
        .expect("read-only replay must not need writer ownership")
        .expect("sidecar exists");
    drop(replayed);

    let err = SpineRuntime::load_for_rollout_items_for_writer(&rollout, &[], &[])
        .expect_err("live replay must fail fast while another writer owns the sidecar");
    assert!(
        err.to_string()
            .contains("already owned by another live Codex process"),
        "unexpected writer replay lock error: {err}"
    );

    let err =
        SpineRuntime::load_or_create(&rollout, 0).expect_err("second live writer must fail fast");
    assert!(
        err.to_string()
            .contains("already owned by another live Codex process"),
        "unexpected writer lock error: {err}"
    );

    drop(runtime);
    drop(eventually_load_or_create_writer(&rollout, 0));
}

#[test]
fn new_sidecar_initializes_empty_trim_ledger() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let store = SpineStore::create_for_rollout(&rollout).expect("create store");

    assert!(
        store.trim_path_for_test().exists(),
        "new Spine sidecars must publish an empty trim ledger"
    );
    assert!(store.trim_events().expect("trim events").is_empty());
    assert_eq!(store.next_trim_seq().expect("next trim seq"), 0);
}

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
