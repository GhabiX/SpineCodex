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
    let root_mem = MemRecord {
        compact_id: "root-1-0".to_string(),
        kind: MemKind::RootEpoch,
        node: NodeId::root_epoch(1),
        raw_start: 0,
        raw_end: 0,
        context_start: 0,
        context_end: 1,
        raw_live_hash: Some(hash_raw_live(&[])),
        open_input_tokens: None,
        close_input_tokens: None,
        open_context_tokens: None,
        close_context_tokens: None,
        closed_source_suffix_tokens: None,
        closed_memory_context_tokens: None,
        open_context_source: None,
        memory_output_tokens: None,
        body_path: root_body_path.clone(),
        body_hash: sha1_hex(root_body.as_bytes()),
    };
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
    let mem = MemRecord {
        compact_id: "root-1-0".to_string(),
        kind: MemKind::RootEpoch,
        node: NodeId::root_epoch(1),
        raw_start: 0,
        raw_end: 0,
        context_start: 0,
        context_end: 1,
        raw_live_hash: Some(hash_raw_live(&[])),
        open_input_tokens: None,
        close_input_tokens: None,
        open_context_tokens: None,
        close_context_tokens: None,
        closed_source_suffix_tokens: None,
        closed_memory_context_tokens: None,
        open_context_source: None,
        memory_output_tokens: None,
        body_path: body_path.clone(),
        body_hash: sha1_hex(body.as_bytes()),
    };
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
        .append_event(&SpineLedgerEvent::Msg {
            raw_ordinal: 0,
            context_index: 0,
            from_user: true,
            user_anchor: None,
        })
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

#[test]
fn compact_checkpoint_same_boundary_hash_token_seq_multiple_records_fails_closed() {
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
    let mem = MemRecord {
        compact_id: "root-1-0".to_string(),
        kind: MemKind::RootEpoch,
        node: NodeId::root_epoch(1),
        raw_start: 0,
        raw_end: 0,
        context_start: 0,
        context_end: 1,
        raw_live_hash: Some(hash_raw_live(&[])),
        open_input_tokens: None,
        close_input_tokens: None,
        open_context_tokens: None,
        close_context_tokens: None,
        closed_source_suffix_tokens: None,
        closed_memory_context_tokens: None,
        open_context_source: None,
        memory_output_tokens: None,
        body_path: body_path.clone(),
        body_hash: sha1_hex(body.as_bytes()),
    };
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
        .expect("append root compact");

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

#[test]
fn clone_for_rollout_keeps_compact_checkpoint_for_matching_raw_live_hash() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source_rollout = dir.path().join("source.jsonl");
    let target_rollout = dir.path().join("target.jsonl");
    let source = SpineStore::create_for_rollout(&source_rollout).expect("create source store");
    source
        .append_event(&SpineLedgerEvent::Init { raw_start: 0 })
        .expect("append init");
    source
        .append_event(&SpineLedgerEvent::Open {
            child: NodeId::root_epoch(1).child(1),
            boundary: 0,
            index: 0,
            summary: "root".to_string(),
            open_input_tokens: None,
            open_context_tokens: None,
            open_context_source: None,
        })
        .expect("append open");
    let raw_live = vec![true, false];
    let raw_live_hash = hash_raw_live(&raw_live);
    let body = "root compact after rollback hole";
    let body_path = source
        .write_memory_body("root-1-2", body)
        .expect("write source body");
    let mem = MemRecord {
        compact_id: "root-1-2".to_string(),
        kind: MemKind::RootEpoch,
        node: NodeId::root_epoch(1),
        raw_start: 0,
        raw_end: 2,
        context_start: 0,
        context_end: 1,
        raw_live_hash: Some(raw_live_hash.clone()),
        open_input_tokens: None,
        close_input_tokens: None,
        open_context_tokens: None,
        close_context_tokens: None,
        closed_source_suffix_tokens: None,
        closed_memory_context_tokens: None,
        open_context_source: None,
        memory_output_tokens: None,
        body_path: body_path,
        body_hash: sha1_hex(body.as_bytes()),
    };
    source.append_mem(&mem).expect("append mem");
    source
        .append_event(&SpineLedgerEvent::RootCompact {
            node: NodeId::root_epoch(1),
            boundary: 2,
            mem: "root-1-2".to_string(),
            next_open_index: 1,
            raw_live_hash: raw_live_hash.clone(),
            next_open_input_tokens: None,
            next_open_context_tokens: None,
        })
        .expect("append root compact");
    source
        .append_compact_checkpoint(&root_compact_checkpoint_for_memory(
            &source_rollout,
            &mem,
            body,
            2,
            3,
            "memory/source-only.md".to_string(),
        ))
        .expect("append compact checkpoint");

    clone_for_rollout_with_raw_live(&source_rollout, &target_rollout, &raw_live);
    let target = SpineStore::for_rollout(&target_rollout).expect("target store");
    assert_eq!(
        target
            .compact_checkpoints()
            .expect("read target checkpoints")
            .len(),
        1
    );
    target
        .validate_compact_checkpoint_for_boundary(
            &target_rollout,
            &raw_live,
            &[],
            2,
            &[memory_response_item(body)],
        )
        .expect("rollback-hole checkpoint should validate against target sidecar");
}

#[test]
fn clone_boundary_excludes_future_compact_checkpoint_and_memory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source_rollout = dir.path().join("source.jsonl");
    let target_rollout = dir.path().join("target.jsonl");
    let mut raw = Vec::new();
    let mut source_runtime =
        SpineRuntime::load_or_create(&source_rollout, 0).expect("create source runtime");
    append_msg(&mut source_runtime, &mut raw, "boundary-visible");
    let boundary = SpineStore::clone_boundary_for_rollout(&source_rollout, 1)
        .expect("capture boundary")
        .expect("source sidecar exists");

    source_runtime
        .root_compact_with_checkpoint(
            &source_rollout,
            "future compact body".to_string(),
            &raw,
            SpineRootCompactTokenMetadata::default(),
        )
        .expect("future root compact");

    SpineStore::clone_for_rollout_with_raw_live(&boundary, &target_rollout, &[true])
        .expect("clone sidecar");
    let target = SpineStore::for_rollout(&target_rollout).expect("target store");
    assert!(
        target.mems().expect("read target mem records").is_empty(),
        "fork boundary must not clone future memory bodies"
    );
    assert!(
        target
            .compact_checkpoints()
            .expect("read target compact checkpoints")
            .is_empty(),
        "fork boundary must not clone future compact checkpoints"
    );
    assert_eq!(
        target
            .events()
            .expect("read target events")
            .iter()
            .map(|event| event.seq)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
}
