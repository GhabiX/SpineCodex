use super::SpineStore;
use super::checkpoint_proof;
use crate::spine::SpineError;
use crate::spine::compact_checkpoint::SpineCompactCheckpoint;
use crate::spine::compact_checkpoint::compact_checkpoint_replacement_history_hash;
use crate::spine::compact_checkpoint::validate_compact_checkpoint;
use std::collections::BTreeSet;
use std::path::Path;

impl SpineStore {
    pub(crate) fn validate_compact_checkpoint_for_boundary(
        &self,
        rollout_path: &Path,
        raw_live: &[bool],
        raw_items: &[Option<codex_protocol::models::ResponseItem>],
        raw_boundary: u64,
        replacement_history: &[codex_protocol::models::ResponseItem],
    ) -> Result<u64, SpineError> {
        let replacement_history_hash =
            compact_checkpoint_replacement_history_hash(replacement_history)?;
        let checkpoint = unique_compact_checkpoint_for_boundary(
            self.compact_checkpoints()?,
            raw_boundary,
            &replacement_history_hash,
        )?;
        validate_compact_checkpoint(
            &checkpoint,
            rollout_path,
            raw_live,
            raw_items,
            replacement_history,
        )?;
        let events = self.events()?;
        let mems = self.mems()?;
        checkpoint_proof::validate_compact_checkpoint_root_marker(
            &self.root,
            &checkpoint,
            &events,
            &mems,
        )?;
        checkpoint_proof::validate_compact_checkpoint_memory_refs(&self.root, &checkpoint, &mems)?;
        Ok(checkpoint.token_seq)
    }
}

fn unique_compact_checkpoint_for_boundary(
    checkpoints: Vec<SpineCompactCheckpoint>,
    raw_boundary: u64,
    replacement_history_hash: &str,
) -> Result<SpineCompactCheckpoint, SpineError> {
    let checkpoints = checkpoints
        .into_iter()
        .filter(|checkpoint| checkpoint.raw_boundary == raw_boundary)
        .collect::<Vec<_>>();
    if checkpoints.is_empty() {
        return Err(SpineError::InvalidStore(format!(
            "missing spine compact checkpoint at raw boundary {raw_boundary}"
        )));
    }
    let checkpoints = checkpoints
        .into_iter()
        .filter(|checkpoint| {
            checkpoint.replacement_history_hash == replacement_history_hash
                && checkpoint.h_ps_hash == replacement_history_hash
        })
        .collect::<Vec<_>>();
    if checkpoints.is_empty() {
        return Err(SpineError::InvalidStore(format!(
            "spine_jit replacement_history does not match sidecar h(PS) compact checkpoint at raw boundary {raw_boundary}"
        )));
    }
    let token_seqs = checkpoints
        .iter()
        .map(|checkpoint| checkpoint.token_seq)
        .collect::<BTreeSet<_>>();
    if token_seqs.len() != 1 {
        return Err(SpineError::InvalidStore(format!(
            "ambiguous spine compact checkpoint token_seq for raw boundary {raw_boundary}"
        )));
    }
    if checkpoints.len() != 1 {
        return Err(SpineError::InvalidStore(format!(
            "ambiguous spine compact checkpoint proof for raw boundary {raw_boundary}"
        )));
    }
    Ok(checkpoints
        .into_iter()
        .next()
        .expect("checkpoint length checked above"))
}
