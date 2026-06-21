use super::super::SpineCloneBoundary;
use super::super::SpineStore;
use super::super::trim;
use crate::spine::SpineError;
use std::path::Path;

pub(in crate::spine::store::clone_sidecar) fn clone_boundary_for_rollout(
    source_rollout_path: &Path,
    raw_ordinal_limit: u64,
) -> Result<Option<SpineCloneBoundary>, SpineError> {
    if !SpineStore::has_for_rollout(source_rollout_path)? {
        return Ok(None);
    }
    let source = SpineStore::for_rollout(source_rollout_path)?;
    let structural_seq_limit = event_seq_limit_for_clone(&source)?;
    let trim_seq_watermark = source.next_trim_seq()?.checked_sub(1);
    Ok(Some(SpineCloneBoundary {
        source_rollout_path: source_rollout_path.to_path_buf(),
        raw_ordinal_limit,
        structural_seq_limit,
        pressure_seq_watermark: source.next_pressure_seq()?.checked_sub(1),
        trim_seq_watermark,
        trim_toolcall_seq_limit: if source.tree_path().exists() {
            structural_seq_limit
        } else {
            trim_toolcall_seq_limit(&source, trim_seq_watermark)?
        },
    }))
}

pub(in crate::spine::store::clone_sidecar) fn clone_boundary_for_checkpoint(
    source_rollout_path: &Path,
    raw_ordinal: u64,
) -> Result<Option<SpineCloneBoundary>, SpineError> {
    if !SpineStore::has_for_rollout(source_rollout_path)? {
        return Ok(None);
    }
    let source = SpineStore::for_rollout(source_rollout_path)?;
    if !source.tree_path().exists() {
        return trim_only_clone_boundary_for_raw_ordinal(&source, source_rollout_path, raw_ordinal);
    }
    let checkpoint = source.checkpoint_for_raw_ordinal(raw_ordinal)?;
    let structural_seq_limit = checkpoint.token_seq;
    Ok(Some(SpineCloneBoundary {
        source_rollout_path: source_rollout_path.to_path_buf(),
        raw_ordinal_limit: raw_ordinal,
        structural_seq_limit,
        pressure_seq_watermark: checkpoint.pressure_seq_watermark,
        trim_seq_watermark: checkpoint.trim_seq_watermark,
        trim_toolcall_seq_limit: if source.tree_path().exists() {
            structural_seq_limit
        } else {
            trim_toolcall_seq_limit(&source, checkpoint.trim_seq_watermark)?
        },
    }))
}

fn trim_only_clone_boundary_for_raw_ordinal(
    source: &SpineStore,
    source_rollout_path: &Path,
    raw_ordinal: u64,
) -> Result<Option<SpineCloneBoundary>, SpineError> {
    let trim_events = source.trim_events()?;
    let trim_seq_watermark = trim::seq_watermark_for_raw_boundary(&trim_events, raw_ordinal);
    Ok(Some(SpineCloneBoundary {
        source_rollout_path: source_rollout_path.to_path_buf(),
        raw_ordinal_limit: raw_ordinal,
        structural_seq_limit: 0,
        pressure_seq_watermark: None,
        trim_seq_watermark,
        trim_toolcall_seq_limit: trim::toolcall_seq_limit_from_events(
            &trim_events,
            trim_seq_watermark,
        )?,
    }))
}

fn trim_toolcall_seq_limit(
    source: &SpineStore,
    trim_seq_watermark: Option<u64>,
) -> Result<u64, SpineError> {
    trim::toolcall_seq_limit_from_events(&source.trim_events()?, trim_seq_watermark)
}

fn event_seq_limit_for_clone(source: &SpineStore) -> Result<u64, SpineError> {
    if source.tree_path().exists() {
        source.next_event_seq()
    } else {
        Ok(0)
    }
}
