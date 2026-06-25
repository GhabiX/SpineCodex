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
    let body_path = store
        .write_memory_body("root-1-0", body)
        .expect("write body");
    let mem = root_epoch_mem_record("root-1-0", body, body_path.clone());
    store.append_mem(&mem).expect("append mem");
    store
        .append_event(&SpineLedgerEvent::RootCompact {
            node: NodeId::root_epoch(1),
            boundary: 0,
            mem: mem.compact_id.clone(),
            next_open_index: 1,
            raw_live_hash: hash_raw_live(&[]),
            next_open_input_tokens: None,
            next_open_context_tokens: None,
        })
        .expect("append first root compact");
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
