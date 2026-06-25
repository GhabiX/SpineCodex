use super::*;

#[test]
fn replacement_history_memory_ref_span_hash_checked() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let store = SpineStore::create_for_rollout(&rollout).expect("create store");
    store
        .append_event(&SpineLedgerEvent::Init { raw_start: 0 })
        .expect("append init");

    let root_body = "root compact body";
    let root_body_path = store
        .write_memory_body("root-1-0", root_body)
        .expect("write root body");
    let root_mem = root_epoch_mem_record("root-1-0", root_body, root_body_path.clone());
    store.append_mem(&root_mem).expect("append root mem");

    let suffix_body = "suffix memory body";
    let suffix_body_path = store
        .write_memory_body("suffix-1-1", suffix_body)
        .expect("write suffix body");
    let suffix_mem = suffix_mem_record(
        "suffix-1-1",
        NodeId::root_epoch(1).child(1),
        suffix_body,
        suffix_body_path.clone(),
        0..0,
        1..2,
        None,
    );
    store.append_mem(&suffix_mem).expect("append suffix mem");
    store
        .append_event(&SpineLedgerEvent::RootCompact {
            node: NodeId::root_epoch(1),
            boundary: 0,
            mem: root_mem.compact_id.clone(),
            next_open_index: 1,
            raw_live_hash: hash_raw_live(&[]),
            next_open_input_tokens: None,
            next_open_context_tokens: None,
        })
        .expect("append root compact");

    let replacement_history = vec![
        memory_response_item(root_body),
        memory_response_item(suffix_body),
    ];
    let replacement_history_hash =
        hash_response_items(&replacement_history).expect("hash replacement_history");
    let mut checkpoint =
        root_compact_checkpoint_for_memory(&rollout, &root_mem, root_body, 1, 2, root_body_path);
    checkpoint.context_len = replacement_history.len();
    checkpoint.h_ps_hash = replacement_history_hash.clone();
    checkpoint.replacement_history_hash = replacement_history_hash;
    checkpoint
        .memory_item_refs
        .push(memory_item_ref_for_body(&suffix_mem, 1, suffix_body));
    checkpoint.memory_refs.push(checkpoint_memory_ref_for_mem(
        &suffix_mem,
        suffix_body_path,
        0,
        0,
        1,
    ));
    store
        .append_compact_checkpoint(&checkpoint)
        .expect("append corrupted compact checkpoint");

    let err = store
        .validate_compact_checkpoint_for_boundary(&rollout, &[], &[], 0, &replacement_history)
        .expect_err("corrupted suffix memory span must fail closed");
    assert!(
        err.to_string()
            .contains("does not match committed memory record"),
        "unexpected checkpoint validation error: {err}"
    );
}
