use super::SpineCloneBoundary;
use super::SpineStore;
use super::clone_rewrite;
use super::locator;
use crate::spine::SpineError;
use crate::spine::checkpoint::SpineCheckpoint;
use crate::spine::compact_checkpoint::SpineCompactCheckpoint;
use crate::spine::model::LoggedSpineLedgerEvent;
use crate::spine::model::LoggedTrimEvent;
use crate::spine::model::MemRecord;
use crate::spine::model::RawMask;
use crate::spine::model::SpineCommitMarker;
use std::collections::BTreeMap;
use std::path::Path;

mod boundary;
mod checkpoints;
mod events;
mod memory_copy;
mod memory_ids;
mod side_ledgers;

impl SpineStore {
    pub(crate) fn clone_boundary_for_rollout(
        source_rollout_path: &Path,
        raw_ordinal_limit: u64,
    ) -> Result<Option<SpineCloneBoundary>, SpineError> {
        boundary::clone_boundary_for_rollout(source_rollout_path, raw_ordinal_limit)
    }

    pub(crate) fn clone_boundary_for_checkpoint(
        source_rollout_path: &Path,
        raw_ordinal: u64,
    ) -> Result<Option<SpineCloneBoundary>, SpineError> {
        boundary::clone_boundary_for_checkpoint(source_rollout_path, raw_ordinal)
    }

    pub(crate) fn clone_for_rollout_with_raw_live(
        boundary: &SpineCloneBoundary,
        target_rollout_path: &Path,
        raw_live: &[bool],
    ) -> Result<(), SpineError> {
        if !Self::has_for_rollout(&boundary.source_rollout_path)? {
            return Ok(());
        }
        if Self::has_for_rollout(target_rollout_path)? {
            return Ok(());
        }
        let raw_ordinal_limit = usize::try_from(boundary.raw_ordinal_limit).map_err(|_| {
            SpineError::InvalidEvent("clone raw ordinal boundary overflow".to_string())
        })?;
        if raw_ordinal_limit > raw_live.len() {
            return Err(SpineError::InvalidEvent(
                "clone raw ordinal boundary exceeds raw live length".to_string(),
            ));
        }
        let source = Self::for_rollout(&boundary.source_rollout_path)?;
        let staging_root = locator::create_unpublished_clone_root(target_rollout_path)?;
        let target_root = staging_root.clone();
        let target = Self::from_root(staging_root.clone());

        let result = clone_for_rollout_into_store(
            &source,
            &target,
            &target_root,
            boundary,
            target_rollout_path,
            raw_live,
            raw_ordinal_limit,
        )
        .and_then(|()| locator::publish_unpublished_clone(target_rollout_path, &staging_root));
        if result.is_err() {
            locator::discard_unpublished_sidecar(&staging_root);
        }
        result
    }
}

fn clone_for_rollout_into_store(
    source: &SpineStore,
    target: &SpineStore,
    target_root: &Path,
    boundary: &SpineCloneBoundary,
    target_rollout_path: &Path,
    raw_live: &[bool],
    raw_ordinal_limit: usize,
) -> Result<(), SpineError> {
    let source_raw_live = &raw_live[..raw_ordinal_limit];
    let mask = RawMask::new(source_raw_live);
    target.ensure_trim_ledger_exists()?;
    let selected = SelectedCloneRecords::from_source(source, boundary, source_raw_live, mask)?;
    for event in &selected.events {
        target.append_logged_event(event)?;
    }
    let mut required_memory_ids = memory_ids::required_memory_ids_for_cloned_events(
        &selected.events,
        &selected.source_mems,
        mask,
    )?;
    memory_ids::add_required_memory_refs(
        &mut required_memory_ids,
        &selected.compact_checkpoints,
        &selected.checkpoints,
        &selected.commit_markers,
    );
    side_ledgers::copy_pressure_and_trim(
        source,
        target,
        selected.source_trim_events,
        boundary,
        source_raw_live,
        mask,
    )?;
    let cloned_memory_paths = memory_copy::copy_required_memories(
        source,
        target,
        selected.source_mems,
        &required_memory_ids,
        mask,
    )?;
    install_cloned_proof_artifacts(
        target,
        target_root,
        target_rollout_path,
        selected.compact_checkpoints,
        selected.checkpoints,
        selected.commit_markers,
        &cloned_memory_paths,
    )
}

fn install_cloned_proof_artifacts(
    target: &SpineStore,
    target_root: &Path,
    target_rollout_path: &Path,
    compact_checkpoints: Vec<SpineCompactCheckpoint>,
    checkpoints: Vec<SpineCheckpoint>,
    commit_markers: Vec<SpineCommitMarker>,
    cloned_memory_paths: &BTreeMap<String, String>,
) -> Result<(), SpineError> {
    for checkpoint in compact_checkpoints {
        let checkpoint = clone_rewrite::clone_compact_checkpoint_for_target(
            checkpoint,
            target_rollout_path,
            cloned_memory_paths,
        )?;
        target.append_compact_checkpoint(&checkpoint)?;
    }
    for checkpoint in checkpoints {
        let checkpoint = clone_rewrite::clone_checkpoint_for_target(
            checkpoint,
            target_rollout_path,
            target_root,
            cloned_memory_paths,
        )?;
        target.write_checkpoint(&checkpoint)?;
    }
    for marker in commit_markers {
        let marker = clone_rewrite::clone_commit_marker_for_target(marker, cloned_memory_paths)?;
        target.append_commit_marker(&marker)?;
    }
    Ok(())
}

struct SourceCloneRecords {
    events: Vec<LoggedSpineLedgerEvent>,
    mems: Vec<MemRecord>,
    checkpoints: Vec<SpineCheckpoint>,
    compact_checkpoints: Vec<SpineCompactCheckpoint>,
    commit_markers: Vec<SpineCommitMarker>,
    trim_events: Vec<LoggedTrimEvent>,
}

struct SelectedCloneRecords {
    events: Vec<LoggedSpineLedgerEvent>,
    source_mems: Vec<MemRecord>,
    checkpoints: Vec<SpineCheckpoint>,
    compact_checkpoints: Vec<SpineCompactCheckpoint>,
    commit_markers: Vec<SpineCommitMarker>,
    source_trim_events: Vec<LoggedTrimEvent>,
}

impl SelectedCloneRecords {
    fn from_source(
        source: &SpineStore,
        boundary: &SpineCloneBoundary,
        source_raw_live: &[bool],
        mask: RawMask<'_>,
    ) -> Result<Self, SpineError> {
        let source_records = SourceCloneRecords::read(source)?;
        let source_events_by_seq = source_records
            .events
            .iter()
            .map(|event| (event.seq, event))
            .collect::<BTreeMap<_, _>>();
        let checkpoints = checkpoints::select_cloned_checkpoints(
            source_records.checkpoints,
            boundary,
            source_raw_live,
        )?;
        let compact_checkpoints = checkpoints::select_cloned_compact_checkpoints(
            source_records.compact_checkpoints,
            boundary,
            source_raw_live,
        )?;
        let (commit_markers, all_marker_structural_event_seqs) =
            events::select_cloned_commit_markers(
                source_records.commit_markers,
                &source_events_by_seq,
                boundary,
                source_raw_live,
                mask,
            )?;
        let events = events::select_cloned_events(
            source_records.events,
            &commit_markers,
            &all_marker_structural_event_seqs,
            boundary,
            mask,
        )?;
        Ok(Self {
            events,
            source_mems: source_records.mems,
            checkpoints,
            compact_checkpoints,
            commit_markers,
            source_trim_events: source_records.trim_events,
        })
    }
}

impl SourceCloneRecords {
    fn read(source: &SpineStore) -> Result<Self, SpineError> {
        let clone_jit_records = source.tree_path().exists();
        let events = read_jit_records(clone_jit_records, || source.events())?;
        let mems = source.mems()?;
        let checkpoints = read_jit_records(clone_jit_records, || source.checkpoints())?;
        let compact_checkpoints =
            read_jit_records(clone_jit_records, || source.compact_checkpoints())?;
        let commit_markers = read_jit_records(clone_jit_records, || source.commit_markers())?;
        let trim_events = source.trim_events()?;
        Ok(Self {
            events,
            mems,
            checkpoints,
            compact_checkpoints,
            commit_markers,
            trim_events,
        })
    }
}

fn read_jit_records<T>(
    clone_jit_records: bool,
    read: impl FnOnce() -> Result<Vec<T>, SpineError>,
) -> Result<Vec<T>, SpineError> {
    if clone_jit_records {
        read()
    } else {
        Ok(Vec::new())
    }
}
