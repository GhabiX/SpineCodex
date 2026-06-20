use crate::spine::SpineError;
use crate::spine::model::COMMIT_MARKER_VERSION;
use crate::spine::model::LoggedSpineLedgerEvent;
use crate::spine::model::MemRecord;
use crate::spine::model::SpineCommitKindMarker;
use crate::spine::model::SpineCommitMarker;
use crate::spine::model::SpineLedgerEvent;
use std::collections::BTreeMap;
use std::path::Path;

mod event_shape;
mod memory_refs;

pub(super) use event_shape::validate_commit_marker_events;
pub(super) use memory_refs::commit_marker_allowed_by_source_live;

pub(super) fn validate_markers_for_replay(
    store_root: &Path,
    markers: &[SpineCommitMarker],
    events: &[LoggedSpineLedgerEvent],
    mems: &[MemRecord],
    raw_live: &[bool],
    min_seq: Option<u64>,
    max_seq: Option<u64>,
) -> Result<(), SpineError> {
    let events_by_seq = events
        .iter()
        .map(|event| (event.seq, event))
        .collect::<BTreeMap<_, _>>();
    let mut markers_by_start = BTreeMap::new();
    for marker in markers {
        validate_commit_marker_record(marker)?;
        if !marker_in_replay_range(marker, min_seq, max_seq) {
            continue;
        }
        if markers_by_start
            .insert(marker.token_seq_start, marker)
            .is_some()
        {
            return Err(SpineError::InvalidStore(format!(
                "ambiguous Spine commit marker at token_seq {}",
                marker.token_seq_start
            )));
        }
        validate_commit_marker_events(marker, &events_by_seq)?;
        memory_refs::validate_commit_marker_memory_refs(store_root, marker, mems, raw_live)?;
    }

    for event in events {
        if !event_seq_in_replay_range(event.seq, min_seq, max_seq) {
            continue;
        }
        match &event.event {
            SpineLedgerEvent::Close { .. } => match markers_by_start.get(&event.seq) {
                Some(marker)
                    if matches!(
                        marker.kind,
                        SpineCommitKindMarker::Close | SpineCommitKindMarker::CloseThenOpen
                    ) => {}
                Some(marker) => {
                    return Err(SpineError::InvalidStore(format!(
                        "Spine commit marker {} at token_seq {} does not commit Close",
                        marker.op_id, event.seq
                    )));
                }
                None => {
                    return Err(SpineError::InvalidStore(format!(
                        "missing Spine commit marker for Close ledger event at token_seq {}",
                        event.seq
                    )));
                }
            },
            SpineLedgerEvent::RootCompact { .. } => match markers_by_start.get(&event.seq) {
                Some(marker) if marker.kind == SpineCommitKindMarker::RootCompact => {}
                Some(marker) => {
                    return Err(SpineError::InvalidStore(format!(
                        "Spine commit marker {} at token_seq {} does not commit RootCompact",
                        marker.op_id, event.seq
                    )));
                }
                None => {
                    return Err(SpineError::InvalidStore(format!(
                        "missing Spine commit marker for RootCompact ledger event at token_seq {}",
                        event.seq
                    )));
                }
            },
            SpineLedgerEvent::Init { .. }
            | SpineLedgerEvent::Msg { .. }
            | SpineLedgerEvent::ToolCall { .. }
            | SpineLedgerEvent::Open { .. }
            | SpineLedgerEvent::OpenContextBaseline { .. } => {}
        }
    }
    Ok(())
}

pub(super) fn validate_commit_marker_record(marker: &SpineCommitMarker) -> Result<(), SpineError> {
    if marker.version != COMMIT_MARKER_VERSION {
        return Err(SpineError::InvalidStore(format!(
            "unsupported Spine commit marker version {}",
            marker.version
        )));
    }
    if marker.op_id.trim().is_empty() {
        return Err(SpineError::InvalidStore(
            "Spine commit marker op_id must not be empty".to_string(),
        ));
    }
    if marker.token_seq_start >= marker.token_seq_end {
        return Err(SpineError::InvalidStore(format!(
            "invalid Spine commit marker token range {}..{}",
            marker.token_seq_start, marker.token_seq_end
        )));
    }
    if marker.memory_refs.is_empty() {
        return Err(SpineError::InvalidStore(format!(
            "Spine commit marker {} must reference memory artifacts",
            marker.op_id
        )));
    }
    Ok(())
}

fn event_seq_in_replay_range(seq: u64, min_seq: Option<u64>, max_seq: Option<u64>) -> bool {
    min_seq.is_none_or(|min_seq| seq >= min_seq) && max_seq.is_none_or(|max_seq| seq < max_seq)
}

fn marker_in_replay_range(
    marker: &SpineCommitMarker,
    min_seq: Option<u64>,
    max_seq: Option<u64>,
) -> bool {
    min_seq.is_none_or(|min_seq| marker.token_seq_start >= min_seq)
        && max_seq.is_none_or(|max_seq| marker.token_seq_end <= max_seq)
}
