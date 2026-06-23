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
        let selected = checkpoint_in_clone_boundary(
            checkpoint.token_seq,
            checkpoint.raw_ordinal,
            raw_boundary_usize(checkpoint.raw_ordinal, "checkpoint raw ordinal overflow")?,
            &checkpoint.raw_live_hash,
            boundary,
            source_raw_live,
        );
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
        if checkpoint_in_clone_boundary(
            checkpoint.token_seq,
            checkpoint.raw_boundary,
            raw_boundary_usize(
                checkpoint.raw_boundary,
                "compact checkpoint raw boundary overflow",
            )?,
            &checkpoint.raw_live_hash,
            boundary,
            source_raw_live,
        ) {
            cloned.push(checkpoint);
        }
    }
    Ok(cloned)
}

fn raw_boundary_usize(raw_boundary: u64, overflow_message: &str) -> Result<usize, SpineError> {
    usize::try_from(raw_boundary)
        .map_err(|_| SpineError::InvalidEvent(overflow_message.to_string()))
}

fn checkpoint_in_clone_boundary(
    token_seq: u64,
    raw_boundary: u64,
    raw_boundary_usize: usize,
    raw_live_hash: &str,
    boundary: &SpineCloneBoundary,
    source_raw_live: &[bool],
) -> bool {
    token_seq <= boundary.structural_seq_limit
        && raw_boundary <= boundary.raw_ordinal_limit
        && raw_boundary_usize <= source_raw_live.len()
        && raw_live_hash == hash_raw_live(&source_raw_live[..raw_boundary_usize])
}
