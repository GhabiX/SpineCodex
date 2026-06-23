use super::event_shape::validate_commit_marker_events;
use super::memory_refs::validate_commit_marker_memory_refs;
use super::validate_commit_marker_record;
use crate::spine::SpineError;
use crate::spine::model::LoggedSpineLedgerEvent;
use crate::spine::model::MemRecord;
use crate::spine::model::SpineCommitKindMarker;
use crate::spine::model::SpineCommitMarker;
use crate::spine::model::SpineLedgerEvent;
use std::collections::BTreeMap;
use std::path::Path;

pub(in crate::spine::store) fn validate_markers_for_replay(
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
        validate_commit_marker_memory_refs(store_root, marker, mems, raw_live)?;
    }

    validate_committed_events_have_markers(events, &markers_by_start, min_seq, max_seq)
}

fn validate_committed_events_have_markers(
    events: &[LoggedSpineLedgerEvent],
    markers_by_start: &BTreeMap<u64, &SpineCommitMarker>,
    min_seq: Option<u64>,
    max_seq: Option<u64>,
) -> Result<(), SpineError> {
    for event in events {
        if !event_seq_in_replay_range(event.seq, min_seq, max_seq) {
            continue;
        }
        match &event.event {
            SpineLedgerEvent::Close { .. } => {
                validate_event_marker_kind(event, markers_by_start, "Close", |kind| {
                    matches!(
                        kind,
                        SpineCommitKindMarker::Close | SpineCommitKindMarker::CloseThenOpen
                    )
                })?
            }
            SpineLedgerEvent::RootCompact { .. } => {
                validate_event_marker_kind(event, markers_by_start, "RootCompact", |kind| {
                    kind == SpineCommitKindMarker::RootCompact
                })?
            }
            SpineLedgerEvent::Init { .. }
            | SpineLedgerEvent::Msg { .. }
            | SpineLedgerEvent::ToolCall { .. }
            | SpineLedgerEvent::Open { .. }
            | SpineLedgerEvent::OpenContextBaseline { .. } => {}
        }
    }
    Ok(())
}

fn validate_event_marker_kind(
    event: &LoggedSpineLedgerEvent,
    markers_by_start: &BTreeMap<u64, &SpineCommitMarker>,
    event_label: &str,
    accepts: impl FnOnce(SpineCommitKindMarker) -> bool,
) -> Result<(), SpineError> {
    match markers_by_start.get(&event.seq) {
        Some(marker) if accepts(marker.kind) => {}
        Some(marker) => {
            return Err(SpineError::InvalidStore(format!(
                "Spine commit marker {} at token_seq {} does not commit {}",
                marker.op_id, event.seq, event_label
            )));
        }
        None => {
            return Err(SpineError::InvalidStore(format!(
                "missing Spine commit marker for {} ledger event at token_seq {}",
                event_label, event.seq
            )));
        }
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
