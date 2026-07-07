use super::*;

pub(crate) fn write_root_compact_memory(
    store: &SpineStore,
    compact_id: &str,
    body: &str,
    raw_range: std::ops::Range<u64>,
    raw_live_hash: String,
) -> (String, MemRecord) {
    let body_path = store
        .write_memory_body(compact_id, body)
        .expect("write body");
    let mem = root_epoch_mem_record_with_raw_live(
        compact_id,
        body,
        body_path.clone(),
        raw_range,
        raw_live_hash,
    );
    store.append_mem(&mem).expect("append mem");
    (body_path, mem)
}

pub(crate) fn append_root_compact_memory_and_marker(
    store: &SpineStore,
    compact_id: &str,
    body: &str,
    raw_range: std::ops::Range<u64>,
    raw_live_hash: String,
) -> (String, MemRecord) {
    let (body_path, mem) =
        write_root_compact_memory(store, compact_id, body, raw_range, raw_live_hash.clone());
    store
        .append_event(&SpineLedgerEvent::RootCompact {
            node: NodeId::root_epoch(1),
            boundary: mem.raw_end,
            mem: mem.compact_id.clone(),
            next_open_index: 1,
            raw_live_hash,
            next_open_input_tokens: None,
            next_open_context_tokens: None,
        })
        .expect("append root compact");
    (body_path, mem)
}

pub(crate) fn append_default_root_compact_memory_and_marker(
    store: &SpineStore,
    compact_id: &str,
    body: &str,
) -> (String, MemRecord) {
    append_root_compact_memory_and_marker(store, compact_id, body, 0..0, hash_raw_live(&[]))
}

pub(crate) fn root_compact_checkpoint_for_memory(
    rollout_path: &std::path::Path,
    mem: &MemRecord,
    body: &str,
    root_event_seq: u64,
    token_seq: u64,
    body_path: String,
) -> SpineCompactCheckpoint {
    let replacement_history = root_memory_replacement_history(mem, body);
    let replacement_history_hash =
        hash_response_items(&replacement_history).expect("hash replacement_history");
    let memory_item_refs = memory_item_refs_for_items(mem, 0, &replacement_history);
    SpineCompactCheckpoint {
        version: CHECKPOINT_VERSION,
        rollout_path: rollout_path.display().to_string(),
        raw_boundary: mem.raw_end,
        token_seq,
        raw_live_hash: mem
            .raw_live_hash
            .clone()
            .expect("root compact memory carries raw live hash"),
        context_len: replacement_history.len(),
        h_ps_hash: replacement_history_hash.clone(),
        replacement_history_hash,
        response_item_refs: Vec::new(),
        memory_item_refs,
        memory_refs: vec![checkpoint_memory_ref_for_mem(
            mem,
            body_path,
            mem.context_start,
            root_event_seq,
            token_seq,
        )],
    }
}

fn root_memory_replacement_history(mem: &MemRecord, body: &str) -> Vec<ResponseItem> {
    let Some(expected_count) = mem.rendered_context_item_count else {
        return vec![memory_response_item(body)];
    };
    let items: Vec<ResponseItem> = serde_json::from_str(body).expect("root body JSON");
    assert_eq!(items.len(), expected_count, "root rendered item count");
    assert!(
        !items.is_empty(),
        "root replacement history must not be empty"
    );
    items
}

fn memory_item_refs_for_items(
    mem: &MemRecord,
    context_start: usize,
    items: &[ResponseItem],
) -> Vec<CompactCheckpointMemoryItemRef> {
    items
        .iter()
        .enumerate()
        .map(|(offset, item)| CompactCheckpointMemoryItemRef {
            compact_id: mem.compact_id.clone(),
            context_index: context_start + offset,
            item_hash: hash_response_items(std::slice::from_ref(item)).expect("hash memory item"),
        })
        .collect()
}

pub(crate) fn memory_item_ref_for_body(
    mem: &MemRecord,
    context_index: usize,
    body: &str,
) -> CompactCheckpointMemoryItemRef {
    CompactCheckpointMemoryItemRef {
        compact_id: mem.compact_id.clone(),
        context_index,
        item_hash: hash_response_items(&[memory_response_item(body)]).expect("hash memory item"),
    }
}

pub(crate) fn checkpoint_memory_ref_for_mem(
    mem: &MemRecord,
    body_path: String,
    source_context_start: usize,
    source_token_seq_start: u64,
    source_token_seq_end: u64,
) -> CheckpointMemoryRef {
    CheckpointMemoryRef {
        compact_id: mem.compact_id.clone(),
        node_id: mem.node.to_string(),
        body_path,
        body_hash: mem.body_hash.clone(),
        source_raw_start: mem.raw_start,
        source_raw_end: mem.raw_end,
        source_context_start,
        source_context_end: mem.context_end,
        source_token_seq_start,
        source_token_seq_end,
        raw_live_hash: mem.raw_live_hash.clone(),
        rendered_context_item_count: mem.rendered_context_item_count,
        open_input_tokens: mem.open_input_tokens,
        close_input_tokens: mem.close_input_tokens,
        open_context_tokens: mem.open_context_tokens,
        close_context_tokens: mem.close_context_tokens,
        closed_source_suffix_tokens: mem.closed_source_suffix_tokens,
        closed_memory_context_tokens: mem.closed_memory_context_tokens,
        open_context_source: mem.open_context_source,
        memory_output_tokens: mem.memory_output_tokens,
    }
}
