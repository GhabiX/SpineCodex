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
    let suffix_mem = MemRecord {
        compact_id: "suffix-1-1".to_string(),
        kind: MemKind::Suffix,
        node: NodeId::root_epoch(1).child(1),
        raw_start: 0,
        raw_end: 0,
        context_start: 1,
        context_end: 2,
        raw_live_hash: None,
        open_input_tokens: None,
        close_input_tokens: None,
        open_context_tokens: None,
        close_context_tokens: None,
        closed_source_suffix_tokens: None,
        closed_memory_context_tokens: None,
        open_context_source: None,
        memory_output_tokens: None,
        body_path: suffix_body_path.clone(),
        body_hash: sha1_hex(suffix_body.as_bytes()),
    };
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
        .push(CompactCheckpointMemoryItemRef {
            compact_id: suffix_mem.compact_id.clone(),
            context_index: 1,
            item_hash: hash_response_items(&[memory_response_item(suffix_body)])
                .expect("hash suffix memory item"),
        });
    checkpoint.memory_refs.push(CheckpointMemoryRef {
        compact_id: suffix_mem.compact_id.clone(),
        node_id: suffix_mem.node.to_string(),
        body_path: suffix_body_path,
        body_hash: suffix_mem.body_hash.clone(),
        source_raw_start: suffix_mem.raw_start,
        source_raw_end: suffix_mem.raw_end,
        source_context_start: 0,
        source_context_end: suffix_mem.context_end,
        source_token_seq_start: 0,
        source_token_seq_end: 1,
        open_input_tokens: suffix_mem.open_input_tokens,
        close_input_tokens: suffix_mem.close_input_tokens,
        open_context_tokens: suffix_mem.open_context_tokens,
        close_context_tokens: suffix_mem.close_context_tokens,
        closed_source_suffix_tokens: suffix_mem.closed_source_suffix_tokens,
        closed_memory_context_tokens: suffix_mem.closed_memory_context_tokens,
        open_context_source: suffix_mem.open_context_source,
        memory_output_tokens: suffix_mem.memory_output_tokens,
    });
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
