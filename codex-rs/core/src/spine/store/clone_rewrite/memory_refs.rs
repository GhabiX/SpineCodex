use crate::spine::SpineError;
use crate::spine::checkpoint::CheckpointMemoryRef;
use crate::spine::model::SpineCommitMemoryRef;
use std::collections::BTreeMap;

pub(super) fn clone_compact_checkpoint_memory_refs(
    memory_refs: Vec<CheckpointMemoryRef>,
    cloned_memory_paths: &BTreeMap<String, String>,
) -> Result<Vec<CheckpointMemoryRef>, SpineError> {
    let mut cloned_refs = Vec::with_capacity(memory_refs.len());
    for memory in memory_refs {
        let body_path = cloned_memory_body_path(
            cloned_memory_paths,
            &memory.compact_id,
            "compact checkpoint references uncloned memory",
        )?;
        cloned_refs.push(CheckpointMemoryRef {
            body_path,
            ..memory
        });
    }
    Ok(cloned_refs)
}

pub(super) fn clone_commit_marker_memory_refs(
    memory_refs: Vec<SpineCommitMemoryRef>,
    cloned_memory_paths: &BTreeMap<String, String>,
) -> Result<Vec<SpineCommitMemoryRef>, SpineError> {
    let mut cloned_refs = Vec::with_capacity(memory_refs.len());
    for memory in memory_refs {
        let body_path = cloned_memory_body_path(
            cloned_memory_paths,
            &memory.compact_id,
            "Spine commit marker references uncloned memory",
        )?;
        cloned_refs.push(SpineCommitMemoryRef {
            body_path,
            ..memory
        });
    }
    Ok(cloned_refs)
}

fn cloned_memory_body_path(
    cloned_memory_paths: &BTreeMap<String, String>,
    compact_id: &str,
    error_prefix: &str,
) -> Result<String, SpineError> {
    cloned_memory_paths
        .get(compact_id)
        .cloned()
        .ok_or_else(|| SpineError::InvalidStore(format!("{error_prefix} {compact_id}")))
}
