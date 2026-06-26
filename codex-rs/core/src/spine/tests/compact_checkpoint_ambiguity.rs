use super::*;

#[test]
fn compact_checkpoint_same_boundary_hash_multiple_token_seq_fails_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let store = SpineStore::create_for_rollout(&rollout).expect("create store");
    store
        .append_event(&SpineLedgerEvent::Init { raw_start: 0 })
        .expect("append init");
    let body = "root compact body";
    let (body_path, mem) = append_default_root_compact_memory_and_marker(&store, "root-1-0", body);
    store
        .append_compact_checkpoint(&root_compact_checkpoint_for_memory(
            &rollout,
            &mem,
            body,
            1,
            2,
            body_path.clone(),
        ))
        .expect("append valid compact checkpoint");
    store
        .append_event(&user_msg_event(0, 0))
        .expect("append non-root marker at second checkpoint predecessor");
    store
        .append_compact_checkpoint(&root_compact_checkpoint_for_memory(
            &rollout, &mem, body, 3, 4, body_path,
        ))
        .expect("append ambiguous newer compact checkpoint");

    let err = store
        .validate_compact_checkpoint_for_boundary(
            &rollout,
            &[],
            &[],
            0,
            &[memory_response_item(body)],
        )
        .expect_err("multiple compact token seq candidates must fail closed");
    assert!(
        err.to_string()
            .contains("ambiguous spine compact checkpoint token_seq"),
        "unexpected checkpoint validation error: {err}"
    );
}
