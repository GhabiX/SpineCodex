use super::SpineCloneBoundary;
use super::SpineStore;
use super::clone_rewrite;
use super::locator;
use crate::ForkSnapshot;
use crate::rollout::truncation;
use crate::session::spine_raw_items_after_rollback;
use crate::spine::SpineError;
use crate::spine::checkpoint::SpineCheckpoint;
use crate::spine::compact_checkpoint::SpineCompactCheckpoint;
use crate::spine::model::LoggedSpineLedgerEvent;
use crate::spine::model::LoggedTrimEvent;
use crate::spine::model::MemRecord;
use crate::spine::model::RawMask;
use crate::spine::model::SpineCommitMarker;
use crate::tasks::InterruptedTurnHistoryMarker;
use crate::tasks::interrupted_turn_history_marker;
use codex_app_server_protocol::ThreadHistoryBuilder;
use codex_app_server_protocol::TurnStatus;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::InitialHistory;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::TurnAbortReason;
use codex_protocol::protocol::TurnAbortedEvent;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
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

    pub(crate) fn clone_boundary_for_fork(
        source_rollout_path: &Path,
        snapshot: ForkSnapshot,
        source_history: &InitialHistory,
        forked_history: &InitialHistory,
    ) -> Result<Option<SpineCloneBoundary>, SpineError> {
        let source_raw_len = raw_source_len_for_fork(source_history)?;
        match snapshot {
            ForkSnapshot::Interrupted => {
                Self::clone_boundary_for_rollout(source_rollout_path, source_raw_len)
            }
            ForkSnapshot::TruncateBeforeNthUserMessage(_) => {
                let raw_ordinal = raw_source_len_for_fork(forked_history)?;
                if raw_ordinal == source_raw_len {
                    Self::clone_boundary_for_rollout(source_rollout_path, source_raw_len)
                } else {
                    Self::clone_boundary_for_checkpoint(source_rollout_path, raw_ordinal)
                }
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn snapshot_turn_state(history: &InitialHistory) -> SnapshotTurnState {
        snapshot_turn_state(history)
    }

    #[cfg(test)]
    pub(crate) fn truncate_before_nth_user_message(
        history: InitialHistory,
        n: usize,
        snapshot_state: &SnapshotTurnState,
    ) -> InitialHistory {
        truncate_before_nth_user_message(history, n, snapshot_state)
    }

    #[cfg(test)]
    pub(crate) fn append_interrupted_boundary(
        history: InitialHistory,
        turn_id: Option<String>,
        interrupted_marker: InterruptedTurnHistoryMarker,
    ) -> InitialHistory {
        append_interrupted_boundary(history, turn_id, interrupted_marker)
    }

    pub(crate) fn fork_history_from_snapshot(
        snapshot: ForkSnapshot,
        history: InitialHistory,
        interrupted_marker: InterruptedTurnHistoryMarker,
    ) -> InitialHistory {
        let snapshot_state = snapshot_turn_state(&history);
        match snapshot {
            ForkSnapshot::TruncateBeforeNthUserMessage(nth_user_message) => {
                truncate_before_nth_user_message(history, nth_user_message, &snapshot_state)
            }
            ForkSnapshot::Interrupted => {
                let history = match history {
                    InitialHistory::New => InitialHistory::New,
                    InitialHistory::Cleared => InitialHistory::Cleared,
                    InitialHistory::Forked(history) => InitialHistory::Forked(history),
                    InitialHistory::Resumed(resumed) => InitialHistory::Forked(resumed.history),
                };
                if snapshot_state.ends_mid_turn {
                    append_interrupted_boundary(
                        history,
                        snapshot_state.active_turn_id,
                        interrupted_marker,
                    )
                } else {
                    history
                }
            }
        }
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
        let source = Self::for_rollout(&boundary.source_rollout_path).map_err(|err| {
            SpineError::InvalidStore(format!(
                "clone source store load failed for {}: {err}",
                boundary.source_rollout_path.display()
            ))
        })?;
        let staging_root =
            locator::create_unpublished_clone_root(target_rollout_path).map_err(|err| {
                SpineError::InvalidStore(format!(
                    "clone staging root allocation failed for {}: {err}",
                    target_rollout_path.display()
                ))
            })?;
        let target = Self::from_root(staging_root.clone());
        let published_root =
            locator::published_root_for_unpublished_clone(target_rollout_path, &staging_root)?;

        let result = clone_for_rollout_into_store(
            &source,
            &target,
            &published_root,
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

fn raw_source_len_for_fork(source_history: &InitialHistory) -> Result<u64, SpineError> {
    u64::try_from(spine_raw_items_after_rollback(&source_history.get_rollout_items()).len())
        .map_err(|_| SpineError::InvalidEvent("source raw length overflow".to_string()))
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
    target.ensure_trim_ledger_exists().map_err(|err| {
        SpineError::InvalidStore(format!(
            "clone target trim ledger init failed for {}: {err}",
            target.root.display()
        ))
    })?;
    let selected = SelectedCloneRecords::from_source(source, boundary, source_raw_live, mask)
        .map_err(|err| SpineError::InvalidStore(format!("clone select records failed: {err}")))?;
    for event in &selected.events {
        target.append_logged_event(event)?;
    }
    let required_memory_ids = required_memory_ids_for_clone(&selected, mask)?;
    side_ledgers::copy_pressure_and_trim(
        source,
        target,
        selected.source_trim_events,
        boundary,
        source_raw_live,
        mask,
    )
    .map_err(|err| SpineError::InvalidStore(format!("clone side ledgers failed: {err}")))?;
    let cloned_memory_paths = memory_copy::copy_required_memories(
        source,
        target,
        selected.source_mems,
        &required_memory_ids,
        mask,
    )
    .map_err(|err| SpineError::InvalidStore(format!("clone memory copy failed: {err}")))?;
    install_cloned_proof_artifacts(
        target,
        target_root,
        target_rollout_path,
        selected.compact_checkpoints,
        selected.checkpoints,
        selected.commit_markers,
        &cloned_memory_paths,
    )
    .map_err(|err| SpineError::InvalidStore(format!("clone proof artifacts failed: {err}")))
}

#[derive(Debug, Eq, PartialEq)]
pub struct SnapshotTurnState {
    pub(crate) ends_mid_turn: bool,
    pub(crate) active_turn_id: Option<String>,
    pub(crate) active_turn_start_index: Option<usize>,
}

pub(super) fn truncate_before_nth_user_message(
    history: InitialHistory,
    n: usize,
    snapshot_state: &SnapshotTurnState,
) -> InitialHistory {
    let items: Vec<RolloutItem> = history.get_rollout_items();
    let user_positions = truncation::user_message_positions_in_rollout(&items);
    let rolled = if snapshot_state.ends_mid_turn && n >= user_positions.len() {
        if let Some(cut_idx) = snapshot_state
            .active_turn_start_index
            .or_else(|| user_positions.last().copied())
        {
            items[..cut_idx].to_vec()
        } else {
            items
        }
    } else {
        truncation::truncate_rollout_before_nth_user_message_from_start(&items, n)
    };

    if rolled.is_empty() {
        InitialHistory::New
    } else {
        InitialHistory::Forked(rolled)
    }
}

pub(super) fn snapshot_turn_state(history: &InitialHistory) -> SnapshotTurnState {
    let rollout_items = history.get_rollout_items();
    let mut builder = ThreadHistoryBuilder::new();
    for item in &rollout_items {
        builder.handle_rollout_item(item);
    }
    let active_turn_id = builder.active_turn_id_if_explicit();
    if builder.has_active_turn() && active_turn_id.is_some() {
        let active_turn_snapshot = builder.active_turn_snapshot();
        if active_turn_snapshot
            .as_ref()
            .is_some_and(|turn| turn.status != TurnStatus::InProgress)
        {
            return SnapshotTurnState {
                ends_mid_turn: false,
                active_turn_id: None,
                active_turn_start_index: None,
            };
        }

        return SnapshotTurnState {
            ends_mid_turn: true,
            active_turn_id,
            active_turn_start_index: builder.active_turn_start_index(),
        };
    }

    let Some(last_user_position) = truncation::user_message_positions_in_rollout(&rollout_items)
        .last()
        .copied()
    else {
        return SnapshotTurnState {
            ends_mid_turn: false,
            active_turn_id: None,
            active_turn_start_index: None,
        };
    };

    SnapshotTurnState {
        ends_mid_turn: !rollout_items[last_user_position + 1..].iter().any(|item| {
            matches!(
                item,
                RolloutItem::EventMsg(EventMsg::TurnComplete(_) | EventMsg::TurnAborted(_))
            )
        }),
        active_turn_id: None,
        active_turn_start_index: None,
    }
}

pub(super) fn append_interrupted_boundary(
    history: InitialHistory,
    turn_id: Option<String>,
    interrupted_marker: InterruptedTurnHistoryMarker,
) -> InitialHistory {
    let aborted_event = RolloutItem::EventMsg(EventMsg::TurnAborted(TurnAbortedEvent {
        turn_id,
        reason: TurnAbortReason::Interrupted,
        completed_at: None,
        duration_ms: None,
    }));

    match history {
        InitialHistory::New | InitialHistory::Cleared => {
            let mut history = Vec::new();
            if let Some(marker) = interrupted_turn_history_marker(interrupted_marker) {
                history.push(RolloutItem::ResponseItem(marker));
            }
            history.push(aborted_event);
            InitialHistory::Forked(history)
        }
        InitialHistory::Forked(mut history) => {
            if let Some(marker) = interrupted_turn_history_marker(interrupted_marker) {
                history.push(RolloutItem::ResponseItem(marker));
            }
            history.push(aborted_event);
            InitialHistory::Forked(history)
        }
        InitialHistory::Resumed(mut resumed) => {
            if let Some(marker) = interrupted_turn_history_marker(interrupted_marker) {
                resumed.history.push(RolloutItem::ResponseItem(marker));
            }
            resumed.history.push(aborted_event);
            InitialHistory::Forked(resumed.history)
        }
    }
}

fn required_memory_ids_for_clone(
    selected: &SelectedCloneRecords,
    mask: RawMask<'_>,
) -> Result<BTreeSet<String>, SpineError> {
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
    Ok(required_memory_ids)
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
