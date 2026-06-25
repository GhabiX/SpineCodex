use super::*;

pub(crate) fn root_compact_checkpoint_for_memory(
    rollout_path: &std::path::Path,
    mem: &MemRecord,
    body: &str,
    root_event_seq: u64,
    token_seq: u64,
    body_path: String,
) -> SpineCompactCheckpoint {
    let replacement_history = vec![memory_response_item(body)];
    let replacement_history_hash =
        hash_response_items(&replacement_history).expect("hash replacement_history");
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
        memory_item_refs: vec![memory_item_ref_for_body(mem, 0, body)],
        memory_refs: vec![checkpoint_memory_ref_for_mem(
            mem,
            body_path,
            mem.context_start,
            root_event_seq,
            token_seq,
        )],
    }
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
