use super::*;
use std::path::PathBuf;

#[path = "runtime_store_writer_fixtures.rs"]
mod runtime_store_writer_fixtures;
pub(super) use runtime_store_writer_fixtures::*;
#[path = "runtime_store_event_fixtures.rs"]
mod runtime_store_event_fixtures;
pub(super) use runtime_store_event_fixtures::*;

// Shared raw/context fixtures.

pub(super) fn rollout_path(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("rollout.jsonl")
}

pub(super) fn clone_for_rollout_with_raw_live(
    source_rollout: &std::path::Path,
    target_rollout: &std::path::Path,
    raw_live: &[bool],
) {
    let boundary = SpineStore::clone_boundary_for_rollout(
        source_rollout,
        u64::try_from(raw_live.len()).expect("raw live len"),
    )
    .expect("capture clone boundary")
    .expect("source sidecar exists");
    SpineStore::clone_for_rollout_with_raw_live(&boundary, target_rollout, raw_live)
        .expect("clone sidecar");
}

pub(super) fn root_compact_checkpoint_for_memory(
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
        memory_item_refs: vec![CompactCheckpointMemoryItemRef {
            compact_id: mem.compact_id.clone(),
            context_index: 0,
            item_hash: hash_response_items(&[memory_response_item(body)])
                .expect("hash memory item"),
        }],
        memory_refs: vec![CheckpointMemoryRef {
            compact_id: mem.compact_id.clone(),
            node_id: mem.node.to_string(),
            body_path,
            body_hash: mem.body_hash.clone(),
            source_raw_start: mem.raw_start,
            source_raw_end: mem.raw_end,
            source_context_start: mem.context_start,
            source_context_end: mem.context_end,
            source_token_seq_start: root_event_seq,
            source_token_seq_end: token_seq,
            open_input_tokens: mem.open_input_tokens,
            close_input_tokens: mem.close_input_tokens,
            open_context_tokens: mem.open_context_tokens,
            close_context_tokens: mem.close_context_tokens,
            closed_source_suffix_tokens: mem.closed_source_suffix_tokens,
            closed_memory_context_tokens: mem.closed_memory_context_tokens,
            open_context_source: mem.open_context_source,
            memory_output_tokens: mem.memory_output_tokens,
        }],
    }
}

pub(super) fn current_context_len(runtime: &SpineRuntime, raw: &[Option<ResponseItem>]) -> usize {
    runtime
        .materialize_history(raw)
        .expect("materialize current h(PS)")
        .len()
}
