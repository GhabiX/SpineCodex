use super::super::SpineCloneBoundary;
use crate::spine::SpineError;
use crate::spine::checkpoint::SpineCheckpoint;
use crate::spine::compact_checkpoint::SpineCompactCheckpoint;
use crate::spine::io::hash_raw_live;

pub(in crate::spine::store::clone_sidecar) fn select_cloned_checkpoints(
    checkpoints: Vec<SpineCheckpoint>,
    boundary: &SpineCloneBoundary,
    source_raw_live: &[bool],
) -> Result<Vec<SpineCheckpoint>, SpineError> {
    let mut cloned = Vec::new();
    for checkpoint in checkpoints {
        let selected = checkpoint_selected_for_clone(
            checkpoint.token_seq,
            checkpoint.raw_ordinal,
            &checkpoint.raw_live_hash,
            "checkpoint raw ordinal overflow",
            boundary,
            source_raw_live,
        )?;
        if checkpoint.checkpoint_id != "initial" && selected {
            cloned.push(checkpoint);
        }
    }
    Ok(cloned)
}

pub(in crate::spine::store::clone_sidecar) fn select_cloned_compact_checkpoints(
    checkpoints: Vec<SpineCompactCheckpoint>,
    boundary: &SpineCloneBoundary,
    source_raw_live: &[bool],
) -> Result<Vec<SpineCompactCheckpoint>, SpineError> {
    let mut cloned = Vec::new();
    for checkpoint in checkpoints {
        if checkpoint_selected_for_clone(
            checkpoint.token_seq,
            checkpoint.raw_boundary,
            &checkpoint.raw_live_hash,
            "compact checkpoint raw boundary overflow",
            boundary,
            source_raw_live,
        )? {
            cloned.push(checkpoint);
        }
    }
    Ok(cloned)
}

fn checkpoint_selected_for_clone(
    token_seq: u64,
    raw_boundary: u64,
    raw_live_hash: &str,
    overflow_message: &str,
    boundary: &SpineCloneBoundary,
    source_raw_live: &[bool],
) -> Result<bool, SpineError> {
    let raw_boundary_usize = usize::try_from(raw_boundary)
        .map_err(|_| SpineError::InvalidEvent(overflow_message.to_string()))?;
    Ok(token_seq <= boundary.structural_seq_limit
        && raw_boundary <= boundary.raw_ordinal_limit
        && raw_boundary_usize <= source_raw_live.len()
        && raw_live_hash == hash_raw_live(&source_raw_live[..raw_boundary_usize]))
}
