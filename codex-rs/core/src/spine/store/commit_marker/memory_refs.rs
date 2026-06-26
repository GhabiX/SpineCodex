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
    if marker.raw_boundary
        > u64::try_from(raw_live.len())
            .map_err(|_| SpineError::InvalidEvent("raw live length overflow".to_string()))?
    {
        return Ok(false);
    }
    if !commit_marker_raw_boundary_proved_by_source_live(marker, raw_live)? {
        return Ok(false);
    }
    marker.memory_refs.iter().try_fold(true, |live, memory| {
        Ok(live && commit_memory_ref_allowed_by_source_live(memory, raw_live)?)
    })
}

pub(in crate::spine::store::commit_marker) fn validate_commit_marker_memory_refs(
    store_root: &Path,
    marker: &SpineCommitMarker,
    mems: &[MemRecord],
    raw_live: &[bool],
) -> Result<(), SpineError> {
    for memory in &marker.memory_refs {
        validate_commit_marker_memory_ref(store_root, marker, memory, mems, raw_live)?;
    }
    if !commit_marker_raw_boundary_proved_by_source_live(marker, raw_live)? {
        return Err(SpineError::InvalidStore(format!(
            "Spine commit marker {} raw boundary {} is not proved by durable raw live state",
            marker.op_id, marker.raw_boundary
        )));
    }
    Ok(())
}

fn commit_marker_raw_boundary_proved_by_source_live(
    marker: &SpineCommitMarker,
    raw_live: &[bool],
) -> Result<bool, SpineError> {
    marker.raw_live_hash.as_deref().map_or(Ok(true), |hash| {
        commit_raw_live_prefix_hash_matches(raw_live, marker.raw_boundary, hash)
    })
}

fn validate_commit_marker_memory_ref(
    store_root: &Path,
    marker: &SpineCommitMarker,
    memory: &SpineCommitMemoryRef,
    mems: &[MemRecord],
    raw_live: &[bool],
) -> Result<(), SpineError> {
    let mem = unique_committed_memory_for_ref(marker, memory, mems)?;
    if !commit_memory_ref_matches_record(memory, mem) {
        return Err(SpineError::InvalidStore(format!(
            "Spine commit marker {} memory ref {} does not match committed memory record",
            marker.op_id, memory.compact_id
        )));
    }
    if !mem.allowed_by(RawMask::new(raw_live))? {
        return Err(SpineError::InvalidStore(format!(
            "memory {} does not cover live raw evidence for Spine commit marker {}",
            mem.compact_id, marker.op_id
        )));
    }
    let body_path = sidecar_store_path(store_root, &memory.body_path);
    memory_body::read_body_with_hash(&body_path, &memory.compact_id, &memory.body_hash)?;
    Ok(())
}

fn unique_committed_memory_for_ref<'a>(
    marker: &SpineCommitMarker,
    memory: &SpineCommitMemoryRef,
    mems: &'a [MemRecord],
) -> Result<&'a MemRecord, SpineError> {
    let mut matching_mems = mems
        .iter()
        .filter(|record| record.compact_id == memory.compact_id);
    let Some(mem) = matching_mems.next() else {
        return Err(SpineError::InvalidStore(format!(
            "Spine commit marker {} references missing memory {}",
            marker.op_id, memory.compact_id
        )));
    };
    if matching_mems.next().is_some() {
        return Err(SpineError::InvalidStore(format!(
            "Spine commit marker {} references ambiguous memory {}",
            marker.op_id, memory.compact_id
        )));
    }
    Ok(mem)
}

fn commit_memory_ref_matches_record(memory: &SpineCommitMemoryRef, mem: &MemRecord) -> bool {
    CommitMemoryRefRecordMatch { memory, mem }.matches()
}

struct CommitMemoryRefRecordMatch<'a> {
    memory: &'a SpineCommitMemoryRef,
    mem: &'a MemRecord,
}

impl CommitMemoryRefRecordMatch<'_> {
    fn matches(&self) -> bool {
        self.memory.kind == self.mem.kind
            && self.memory.node == self.mem.node
            && self.memory.raw_start == self.mem.raw_start
            && self.memory.raw_end == self.mem.raw_end
            && self.memory.context_start == self.mem.context_start
            && self.memory.context_end == self.mem.context_end
            && self.memory.raw_live_hash == self.mem.raw_live_hash
            && self.memory.body_path == self.mem.body_path
            && self.memory.body_hash == self.mem.body_hash
    }
}

fn commit_memory_ref_allowed_by_source_live(
    memory: &SpineCommitMemoryRef,
    raw_live: &[bool],
) -> Result<bool, SpineError> {
    let raw_mask = RawMask::new(raw_live);
    match memory.kind {
        MemKind::Suffix => raw_mask.span_live(memory.raw_start, memory.raw_end),
        MemKind::RootEpoch => memory.raw_live_hash.as_deref().map_or(Ok(false), |hash| {
            commit_raw_live_prefix_hash_matches(raw_live, memory.raw_end, hash)
        }),
    }
}

fn commit_raw_live_prefix_hash_matches(
    raw_live: &[bool],
    boundary: u64,
    expected: &str,
) -> Result<bool, SpineError> {
    RawMask::new(raw_live).prefix_hash_matches_with_overflow(
        boundary,
        expected,
        "raw boundary overflow",
    )
}
