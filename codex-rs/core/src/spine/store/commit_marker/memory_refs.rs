use super::super::mem_lookup::unique_mem_record_by_compact_id;
use super::super::memory_body;
use super::super::sidecar_store_path;
use crate::spine::SpineError;
use crate::spine::model::MemKind;
use crate::spine::model::MemRecord;
use crate::spine::model::RawMask;
use crate::spine::model::SpineCommitMarker;
use crate::spine::model::SpineCommitMemoryRef;
use std::path::Path;

pub(in crate::spine::store) fn commit_marker_allowed_by_source_live(
    marker: &SpineCommitMarker,
    raw_live: &[bool],
) -> Result<bool, SpineError> {
    let raw_mask = RawMask::new(raw_live);
    if marker.raw_boundary
        > u64::try_from(raw_live.len())
            .map_err(|_| SpineError::InvalidEvent("raw live length overflow".to_string()))?
    {
        return Ok(false);
    }
    if !commit_marker_raw_boundary_proved_by_source_live(marker, raw_mask)? {
        return Ok(false);
    }
    marker.memory_refs.iter().try_fold(true, |live, memory| {
        Ok(live && commit_memory_ref_allowed_by_source_live(memory, raw_mask)?)
    })
}

pub(super) fn validate_commit_marker_memory_refs(
    store_root: &Path,
    marker: &SpineCommitMarker,
    mems: &[MemRecord],
    raw_live: &[bool],
) -> Result<(), SpineError> {
    let raw_mask = RawMask::new(raw_live);
    for memory in &marker.memory_refs {
        validate_commit_marker_memory_ref(store_root, marker, memory, mems, raw_mask)?;
    }
    if !commit_marker_raw_boundary_proved_by_source_live(marker, raw_mask)? {
        return Err(SpineError::InvalidStore(format!(
            "Spine commit marker {} raw boundary {} is not proved by durable raw live state",
            marker.op_id, marker.raw_boundary
        )));
    }
    Ok(())
}

fn commit_marker_raw_boundary_proved_by_source_live(
    marker: &SpineCommitMarker,
    raw_mask: RawMask<'_>,
) -> Result<bool, SpineError> {
    marker.raw_live_hash.as_deref().map_or(Ok(true), |hash| {
        raw_mask.prefix_hash_matches_with_overflow(
            marker.raw_boundary,
            hash,
            "raw boundary overflow",
        )
    })
}

fn validate_commit_marker_memory_ref(
    store_root: &Path,
    marker: &SpineCommitMarker,
    memory: &SpineCommitMemoryRef,
    mems: &[MemRecord],
    raw_mask: RawMask<'_>,
) -> Result<(), SpineError> {
    let mem = unique_mem_record_by_compact_id(
        &memory.compact_id,
        mems,
        || {
            format!(
                "Spine commit marker {} references missing memory {}",
                marker.op_id, memory.compact_id
            )
        },
        || {
            format!(
                "Spine commit marker {} references ambiguous memory {}",
                marker.op_id, memory.compact_id
            )
        },
    )?;
    if !commit_memory_ref_matches_record(memory, mem) {
        return Err(SpineError::InvalidStore(format!(
            "Spine commit marker {} memory ref {} does not match committed memory record",
            marker.op_id, memory.compact_id
        )));
    }
    if !mem.allowed_by(raw_mask)? {
        return Err(SpineError::InvalidStore(format!(
            "memory {} does not cover live raw evidence for Spine commit marker {}",
            mem.compact_id, marker.op_id
        )));
    }
    let body_path = sidecar_store_path(store_root, &memory.body_path);
    memory_body::read_body_with_hash(&body_path, &memory.compact_id, &memory.body_hash)?;
    Ok(())
}

fn commit_memory_ref_matches_record(memory: &SpineCommitMemoryRef, mem: &MemRecord) -> bool {
    memory.kind == mem.kind
        && memory.node == mem.node
        && memory.raw_start == mem.raw_start
        && memory.raw_end == mem.raw_end
        && memory.context_start == mem.context_start
        && memory.context_end == mem.context_end
        && memory.rendered_context_item_count == mem.rendered_context_item_count
        && memory.raw_live_hash == mem.raw_live_hash
        && memory.body_path == mem.body_path
        && memory.body_hash == mem.body_hash
}

fn commit_memory_ref_allowed_by_source_live(
    memory: &SpineCommitMemoryRef,
    raw_mask: RawMask<'_>,
) -> Result<bool, SpineError> {
    match memory.kind {
        MemKind::Suffix => memory.raw_live_hash.as_deref().map_or_else(
            || raw_mask.span_live(memory.raw_start, memory.raw_end),
            |hash| {
                raw_mask.prefix_hash_matches_with_overflow(
                    memory.raw_end,
                    hash,
                    "raw boundary overflow",
                )
            },
        ),
        MemKind::RootEpoch => memory.raw_live_hash.as_deref().map_or(Ok(false), |hash| {
            raw_mask.prefix_hash_matches_with_overflow(
                memory.raw_end,
                hash,
                "raw boundary overflow",
            )
        }),
    }
}
