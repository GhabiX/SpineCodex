use super::*;

#[test]
fn compact_checkpoint_same_boundary_hash_token_seq_multiple_records_fails_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let store = SpineStore::create_for_rollout(&rollout).expect("create store");
    store
        .append_event(&SpineLedgerEvent::Init { raw_start: 0 })
        .expect("append init");
    let body = "root compact body";
    let (body_path, mem) = append_default_root_compact_memory_and_marker(&store, "root-1-0", body);

    let mut corrupted =
        root_compact_checkpoint_for_memory(&rollout, &mem, body, 1, 2, body_path.clone());
    corrupted.context_len += 1;
    store
        .append_compact_checkpoint(&corrupted)
        .expect("append corrupted compact checkpoint");
    store
        .append_compact_checkpoint(&root_compact_checkpoint_for_memory(
            &rollout, &mem, body, 1, 2, body_path,
        ))
        .expect("append duplicate valid compact checkpoint");

    let err = store
        .validate_compact_checkpoint_for_boundary(
            &rollout,
            &[],
            &[],
            0,
            &[memory_response_item(body)],
        )
        .expect_err("multiple compact proof records must fail closed");
    assert!(
        err.to_string()
            .contains("ambiguous spine compact checkpoint proof"),
        "unexpected checkpoint validation error: {err}"
    );
}
