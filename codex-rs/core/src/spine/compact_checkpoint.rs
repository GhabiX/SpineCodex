use crate::spine::CHECKPOINT_VERSION;
use crate::spine::SpineError;
use crate::spine::checkpoint::CheckpointMemoryRef;
use crate::spine::checkpoint::collect_checkpoint_refs;
use crate::spine::io::hash_raw_live;
use crate::spine::io::hash_response_items;
use crate::spine::parse_stack::ParseStack;
use codex_protocol::models::ResponseItem;
use serde::Deserialize;
use serde::Serialize;
use std::path::Path;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct SpineCompactCheckpoint {
    pub(super) version: u32,
    pub(super) rollout_path: String,
    pub(super) raw_boundary: u64,
    pub(super) token_seq: u64,
    pub(super) raw_live_hash: String,
    pub(super) context_len: usize,
    pub(super) h_ps_hash: String,
    pub(super) replacement_history_hash: String,
    pub(super) memory_refs: Vec<CheckpointMemoryRef>,
}

pub(super) fn build_compact_checkpoint(
    rollout_path: &Path,
    raw_boundary: u64,
    token_seq: u64,
    raw_live: &[bool],
    parse_stack: &ParseStack,
    context: &[ResponseItem],
    replacement_history: &[ResponseItem],
) -> Result<SpineCompactCheckpoint, SpineError> {
    let raw_boundary_usize = usize::try_from(raw_boundary)
        .map_err(|_| SpineError::InvalidEvent("compact raw boundary overflow".to_string()))?;
    if raw_boundary_usize > raw_live.len() {
        return Err(SpineError::InvalidEvent(
            "compact raw boundary exceeds raw live length".to_string(),
        ));
    }
    let mut tree_meta = Vec::new();
    let mut memory_refs = Vec::new();
    let mut trajs_refs = Vec::new();
    collect_checkpoint_refs(
        &parse_stack.symbols,
        &mut tree_meta,
        &mut memory_refs,
        &mut trajs_refs,
    );
    Ok(SpineCompactCheckpoint {
        version: CHECKPOINT_VERSION,
        rollout_path: rollout_path.display().to_string(),
        raw_boundary,
        token_seq,
        raw_live_hash: hash_raw_live(&raw_live[..raw_boundary_usize]),
        context_len: context.len(),
        h_ps_hash: hash_response_items(context)?,
        replacement_history_hash: hash_response_items(replacement_history)?,
        memory_refs,
    })
}

pub(super) fn validate_compact_checkpoint(
    checkpoint: &SpineCompactCheckpoint,
    rollout_path: &Path,
    raw_live: &[bool],
    replacement_history: &[ResponseItem],
) -> Result<(), SpineError> {
    if checkpoint.version != CHECKPOINT_VERSION {
        return Err(SpineError::InvalidStore(format!(
            "unsupported spine compact checkpoint version {}",
            checkpoint.version
        )));
    }
    let end = usize::try_from(checkpoint.raw_boundary)
        .map_err(|_| SpineError::InvalidEvent("compact raw boundary overflow".to_string()))?;
    if end > raw_live.len() {
        return Err(SpineError::InvalidStore(format!(
            "spine compact checkpoint raw boundary exceeds rollout at {}",
            checkpoint.raw_boundary
        )));
    }
    if checkpoint.rollout_path != rollout_path.display().to_string() {
        return Err(SpineError::InvalidStore(format!(
            "spine compact checkpoint rollout identity mismatch at raw boundary {}",
            checkpoint.raw_boundary
        )));
    }
    if checkpoint.raw_live_hash != hash_raw_live(&raw_live[..end]) {
        return Err(SpineError::InvalidStore(format!(
            "spine compact checkpoint raw boundary hash mismatch at raw boundary {}",
            checkpoint.raw_boundary
        )));
    }
    let replacement_history_hash = hash_response_items(replacement_history)?;
    if checkpoint.replacement_history_hash != replacement_history_hash {
        return Err(SpineError::InvalidStore(format!(
            "spine_task_tree replacement_history does not match sidecar compact checkpoint at raw boundary {}",
            checkpoint.raw_boundary
        )));
    }
    if checkpoint.h_ps_hash != replacement_history_hash {
        return Err(SpineError::InvalidStore(format!(
            "spine compact checkpoint h(PS) hash mismatch at raw boundary {}",
            checkpoint.raw_boundary
        )));
    }
    Ok(())
}

pub(super) fn compact_checkpoint_replacement_history_hash(
    replacement_history: &[ResponseItem],
) -> Result<String, SpineError> {
    hash_response_items(replacement_history)
}
