use super::*;

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
