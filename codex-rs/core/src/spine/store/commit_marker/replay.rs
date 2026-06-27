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
    let replay_range = ReplaySeqRange { min_seq, max_seq };
    let events_by_seq = events
        .iter()
        .map(|event| (event.seq, event))
        .collect::<BTreeMap<_, _>>();
    let mut markers_by_start = BTreeMap::new();
    for marker in markers {
        validate_commit_marker_record(marker)?;
        if !replay_range.contains_marker(marker) {
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
        if !replay_range.contains_event_seq(event.seq) {
            continue;
        }
        if let Some(requirement) = CommittedEventMarkerRequirement::for_event(&event.event) {
            validate_event_marker_kind(event, &markers_by_start, requirement)?;
        }
    }
    Ok(())
}

struct CommittedEventMarkerRequirement {
    event_label: &'static str,
    accepted_kinds: &'static [SpineCommitKindMarker],
}

const CLOSE_EVENT_MARKER_KINDS: &[SpineCommitKindMarker] = &[
    SpineCommitKindMarker::Close,
    SpineCommitKindMarker::CloseThenOpen,
];
const ROOT_COMPACT_EVENT_MARKER_KINDS: &[SpineCommitKindMarker] =
    &[SpineCommitKindMarker::RootCompact];

impl CommittedEventMarkerRequirement {
    fn for_event(event: &SpineLedgerEvent) -> Option<Self> {
        match event {
            SpineLedgerEvent::Close { .. } => Some(Self {
                event_label: "Close",
                accepted_kinds: CLOSE_EVENT_MARKER_KINDS,
            }),
            SpineLedgerEvent::RootCompact { .. } => Some(Self {
                event_label: "RootCompact",
                accepted_kinds: ROOT_COMPACT_EVENT_MARKER_KINDS,
            }),
            SpineLedgerEvent::Init { .. }
            | SpineLedgerEvent::Msg { .. }
            | SpineLedgerEvent::ToolCall { .. }
            | SpineLedgerEvent::Open { .. }
            | SpineLedgerEvent::OpenContextBaseline { .. } => None,
        }
    }
}

fn validate_event_marker_kind(
    event: &LoggedSpineLedgerEvent,
    markers_by_start: &BTreeMap<u64, &SpineCommitMarker>,
    requirement: CommittedEventMarkerRequirement,
) -> Result<(), SpineError> {
    match markers_by_start.get(&event.seq) {
        Some(marker) if requirement.accepted_kinds.contains(&marker.kind) => {}
        Some(marker) => {
            return Err(SpineError::InvalidStore(format!(
                "Spine commit marker {} at token_seq {} does not commit {}",
                marker.op_id, event.seq, requirement.event_label
            )));
        }
        None => {
            return Err(SpineError::InvalidStore(format!(
                "missing Spine commit marker for {} ledger event at token_seq {}",
                requirement.event_label, event.seq
            )));
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct ReplaySeqRange {
    min_seq: Option<u64>,
    max_seq: Option<u64>,
}

impl ReplaySeqRange {
    fn contains_event_seq(self, seq: u64) -> bool {
        self.min_seq.is_none_or(|min_seq| seq >= min_seq)
            && self.max_seq.is_none_or(|max_seq| seq < max_seq)
    }

    fn contains_marker(self, marker: &SpineCommitMarker) -> bool {
        self.min_seq
            .is_none_or(|min_seq| marker.token_seq_start >= min_seq)
            && self
                .max_seq
                .is_none_or(|max_seq| marker.token_seq_end <= max_seq)
    }
}
