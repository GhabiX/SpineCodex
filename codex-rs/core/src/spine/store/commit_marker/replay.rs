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
    let contains_event_seq = |seq| {
        min_seq.is_none_or(|min_seq| seq >= min_seq) && max_seq.is_none_or(|max_seq| seq < max_seq)
    };
    let contains_marker = |marker: &SpineCommitMarker| {
        min_seq.is_none_or(|min_seq| marker.token_seq_start >= min_seq)
            && max_seq.is_none_or(|max_seq| marker.token_seq_end <= max_seq)
    };
    let events_by_seq = events
        .iter()
        .map(|event| (event.seq, event))
        .collect::<BTreeMap<_, _>>();
    let mut markers_by_start = BTreeMap::new();
    for marker in markers {
        validate_commit_marker_record(marker)?;
        if !contains_marker(marker) {
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

    for event in events {
        if !contains_event_seq(event.seq) {
            continue;
        }
        let (event_label, accepted_kinds) = match &event.event {
            SpineLedgerEvent::Close { .. } => ("Close", CLOSE_EVENT_MARKER_KINDS),
            SpineLedgerEvent::RootCompact { .. } => {
                ("RootCompact", ROOT_COMPACT_EVENT_MARKER_KINDS)
            }
            SpineLedgerEvent::Init { .. }
            | SpineLedgerEvent::Msg { .. }
            | SpineLedgerEvent::ToolCall { .. }
            | SpineLedgerEvent::Open { .. }
            | SpineLedgerEvent::OpenContextBaseline { .. } => continue,
        };
        validate_event_marker_kind(event, &markers_by_start, event_label, accepted_kinds)?;
    }
    Ok(())
}

const CLOSE_EVENT_MARKER_KINDS: &[SpineCommitKindMarker] = &[
    SpineCommitKindMarker::Close,
    SpineCommitKindMarker::CloseThenOpen,
];
const ROOT_COMPACT_EVENT_MARKER_KINDS: &[SpineCommitKindMarker] =
    &[SpineCommitKindMarker::RootCompact];

fn validate_event_marker_kind(
    event: &LoggedSpineLedgerEvent,
    markers_by_start: &BTreeMap<u64, &SpineCommitMarker>,
    event_label: &'static str,
    accepted_kinds: &[SpineCommitKindMarker],
) -> Result<(), SpineError> {
    match markers_by_start.get(&event.seq) {
        Some(marker) if accepted_kinds.contains(&marker.kind) => {}
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
