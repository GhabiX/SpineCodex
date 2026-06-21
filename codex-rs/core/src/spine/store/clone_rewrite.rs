use crate::spine::SpineError;
use crate::spine::compact_checkpoint::SpineCompactCheckpoint;
use crate::spine::model::SpineCommitMarker;
use std::collections::BTreeMap;
use std::path::Path;

mod checkpoint;
mod memory_refs;

pub(super) use checkpoint::clone_checkpoint_for_target;

pub(super) fn clone_compact_checkpoint_for_target(
    checkpoint: SpineCompactCheckpoint,
    target_rollout_path: &Path,
    cloned_memory_paths: &BTreeMap<String, String>,
) -> Result<SpineCompactCheckpoint, SpineError> {
    let memory_refs = memory_refs::clone_compact_checkpoint_memory_refs(
        checkpoint.memory_refs,
        cloned_memory_paths,
    )?;
    Ok(SpineCompactCheckpoint {
        rollout_path: target_rollout_path.display().to_string(),
        memory_refs,
        ..checkpoint
    })
}

pub(super) fn clone_commit_marker_for_target(
    marker: SpineCommitMarker,
    cloned_memory_paths: &BTreeMap<String, String>,
) -> Result<SpineCommitMarker, SpineError> {
    let memory_refs =
        memory_refs::clone_commit_marker_memory_refs(marker.memory_refs, cloned_memory_paths)?;
    Ok(SpineCommitMarker {
        memory_refs,
        ..marker
    })
}
