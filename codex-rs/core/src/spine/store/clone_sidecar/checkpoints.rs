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
        let checkpoint_boundary = usize::try_from(checkpoint.raw_ordinal)
            .map_err(|_| SpineError::InvalidEvent("checkpoint raw ordinal overflow".to_string()))?;
        if checkpoint.checkpoint_id != "initial"
            && checkpoint.token_seq <= boundary.structural_seq_limit
            && checkpoint.raw_ordinal <= boundary.raw_ordinal_limit
            && checkpoint_boundary <= source_raw_live.len()
            && checkpoint.raw_live_hash == hash_raw_live(&source_raw_live[..checkpoint_boundary])
        {
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
        let checkpoint_boundary = usize::try_from(checkpoint.raw_boundary).map_err(|_| {
            SpineError::InvalidEvent("compact checkpoint raw boundary overflow".to_string())
        })?;
        if checkpoint.token_seq <= boundary.structural_seq_limit
            && checkpoint.raw_boundary <= boundary.raw_ordinal_limit
            && checkpoint_boundary <= source_raw_live.len()
            && checkpoint.raw_live_hash == hash_raw_live(&source_raw_live[..checkpoint_boundary])
        {
            cloned.push(checkpoint);
        }
    }
    Ok(cloned)
}
