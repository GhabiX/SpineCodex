use crate::spine::SpineError;
use crate::spine::model::LoggedSpineLedgerEvent;
use crate::spine::model::NodeId;
use crate::spine::model::SpineCommitKindMarker;
use crate::spine::model::SpineCommitMarker;
use crate::spine::model::SpineCommitMemoryRef;
use crate::spine::model::SpineLedgerEvent;
use std::collections::BTreeMap;

pub(in crate::spine::store) fn validate_commit_marker_events(
    marker: &SpineCommitMarker,
    events_by_seq: &BTreeMap<u64, &LoggedSpineLedgerEvent>,
) -> Result<(), SpineError> {
    let shape = CommitMarkerShape::for_kind(marker.kind);
    validate_commit_marker_width(marker, shape.width)?;
    (shape.validate_start_event)(marker, events_by_seq)?;
    validate_required_marker_events(marker, events_by_seq, shape.required_events)
}

struct CommitMarkerShape {
    width: u64,
    validate_start_event: CommitMarkerStartEventValidator,
    required_events: &'static [RequiredMarkerEvent],
}

type CommitMarkerStartEventValidator =
    fn(&SpineCommitMarker, &BTreeMap<u64, &LoggedSpineLedgerEvent>) -> Result<(), SpineError>;

#[derive(Clone, Copy)]
struct RequiredMarkerEvent {
    offset: u64,
    kind: RequiredMarkerEventKind,
}

#[derive(Clone, Copy)]
enum RequiredMarkerEventKind {
    SyntheticOpen,
    TrailingToolCall,
}

const NO_REQUIRED_EVENTS: &[RequiredMarkerEvent] = &[];
const CLOSE_REQUIRED_EVENTS: &[RequiredMarkerEvent] = &[RequiredMarkerEvent {
    offset: 1,
    kind: RequiredMarkerEventKind::TrailingToolCall,
}];
const CLOSE_THEN_OPEN_REQUIRED_EVENTS: &[RequiredMarkerEvent] = &[
    RequiredMarkerEvent {
        offset: 1,
        kind: RequiredMarkerEventKind::SyntheticOpen,
    },
    RequiredMarkerEvent {
        offset: 2,
        kind: RequiredMarkerEventKind::TrailingToolCall,
    },
];

impl CommitMarkerShape {
    fn for_kind(kind: SpineCommitKindMarker) -> Self {
        match kind {
            SpineCommitKindMarker::Close => Self {
                width: 2,
                validate_start_event: validate_close_marker_start_event,
                required_events: CLOSE_REQUIRED_EVENTS,
            },
            SpineCommitKindMarker::CloseThenOpen => Self {
                width: 3,
                validate_start_event: validate_close_marker_start_event,
                required_events: CLOSE_THEN_OPEN_REQUIRED_EVENTS,
            },
            SpineCommitKindMarker::RootCompact => Self {
                width: 1,
                validate_start_event: validate_root_compact_shape,
                required_events: NO_REQUIRED_EVENTS,
            },
        }
    }
}

fn validate_required_marker_events(
    marker: &SpineCommitMarker,
    events_by_seq: &BTreeMap<u64, &LoggedSpineLedgerEvent>,
    required_events: &[RequiredMarkerEvent],
) -> Result<(), SpineError> {
    for required in required_events {
        let seq = marker_shape_seq(marker, required.offset)?;
        match required.kind {
            RequiredMarkerEventKind::SyntheticOpen => {
                validate_required_synthetic_open(marker, events_by_seq, seq)?;
            }
            RequiredMarkerEventKind::TrailingToolCall => {
                validate_required_trailing_toolcall(marker, events_by_seq, seq)?;
            }
        }
    }
    Ok(())
}

fn validate_close_marker_start_event(
    marker: &SpineCommitMarker,
    events_by_seq: &BTreeMap<u64, &LoggedSpineLedgerEvent>,
) -> Result<(), SpineError> {
    let (node, boundary) = close_event_at_marker_start(marker, events_by_seq)?;
    validate_close_marker_fields(marker, node, *boundary)
}

fn validate_required_synthetic_open(
    marker: &SpineCommitMarker,
    events_by_seq: &BTreeMap<u64, &LoggedSpineLedgerEvent>,
    seq: u64,
) -> Result<(), SpineError> {
    let Some(open) = events_by_seq.get(&seq) else {
        return Err(SpineError::InvalidStore(format!(
            "Spine commit marker {} is missing synthetic Open at token_seq {}",
            marker.op_id, seq
        )));
    };
    let SpineLedgerEvent::Open { boundary, .. } = &open.event else {
        return Err(SpineError::InvalidStore(format!(
            "Spine commit marker {} is not followed by synthetic Open at token_seq {}",
            marker.op_id, seq
        )));
    };
    validate_marker_raw_boundary(marker, *boundary, "synthetic Open")?;
    Ok(())
}

fn validate_root_compact_shape(
    marker: &SpineCommitMarker,
    events_by_seq: &BTreeMap<u64, &LoggedSpineLedgerEvent>,
) -> Result<(), SpineError> {
    let event = event_at_marker_start(marker, events_by_seq)?;
    let SpineLedgerEvent::RootCompact {
        node,
        boundary,
        mem,
        raw_live_hash,
        ..
    } = &event.event
    else {
        return Err(SpineError::InvalidStore(format!(
            "Spine commit marker {} is not backed by RootCompact at token_seq {}",
            marker.op_id, marker.token_seq_start
        )));
    };
    validate_marker_raw_boundary(marker, *boundary, "RootCompact")?;
    validate_marker_raw_live_hash_matches(marker, raw_live_hash, "RootCompact")?;
    validate_single_memory_ref_matches_root_compact(marker, node, mem)?;
    Ok(())
}

fn marker_shape_seq(marker: &SpineCommitMarker, offset: u64) -> Result<u64, SpineError> {
    marker.token_seq_start.checked_add(offset).ok_or_else(|| {
        SpineError::InvalidEvent("Spine commit marker token seq overflow".to_string())
    })
}

fn close_event_at_marker_start<'a>(
    marker: &SpineCommitMarker,
    events_by_seq: &'a BTreeMap<u64, &LoggedSpineLedgerEvent>,
) -> Result<(&'a NodeId, &'a u64), SpineError> {
    let event = event_at_marker_start(marker, events_by_seq)?;
    let SpineLedgerEvent::Close { node, boundary, .. } = &event.event else {
        return Err(SpineError::InvalidStore(format!(
            "Spine commit marker {} is not backed by Close at token_seq {}",
            marker.op_id, marker.token_seq_start
        )));
    };
    Ok((node, boundary))
}

fn validate_commit_marker_width(marker: &SpineCommitMarker, width: u64) -> Result<(), SpineError> {
    let expected_end = marker_shape_seq(marker, width)?;
    if marker.token_seq_end != expected_end {
        return Err(SpineError::InvalidStore(format!(
            "Spine commit marker {} token range {}..{} has unexpected width {width}",
            marker.op_id, marker.token_seq_start, marker.token_seq_end
        )));
    }
    Ok(())
}

fn validate_marker_raw_boundary(
    marker: &SpineCommitMarker,
    boundary: u64,
    event_label: &str,
) -> Result<(), SpineError> {
    if boundary != marker.raw_boundary {
        return Err(SpineError::InvalidStore(format!(
            "Spine commit marker {} raw boundary {} does not match {} boundary {}",
            marker.op_id, marker.raw_boundary, event_label, boundary
        )));
    }
    Ok(())
}

fn validate_required_trailing_toolcall(
    marker: &SpineCommitMarker,
    events_by_seq: &BTreeMap<u64, &LoggedSpineLedgerEvent>,
    seq: u64,
) -> Result<(), SpineError> {
    if seq.checked_add(1) != Some(marker.token_seq_end) {
        return Err(SpineError::InvalidStore(format!(
            "Spine commit marker {} must end with exactly one trailing ToolCall in token range {}..{}",
            marker.op_id, marker.token_seq_start, marker.token_seq_end
        )));
    }
    let Some(event) = events_by_seq.get(&seq) else {
        return Err(SpineError::InvalidStore(format!(
            "Spine commit marker {} references missing trailing ToolCall at token_seq {}",
            marker.op_id, seq
        )));
    };
    if !matches!(event.event, SpineLedgerEvent::ToolCall { .. }) {
        return Err(SpineError::InvalidStore(format!(
            "Spine commit marker {} trailing event at token_seq {} is not ToolCall",
            marker.op_id, seq
        )));
    }
    Ok(())
}

fn event_at_marker_start<'a>(
    marker: &SpineCommitMarker,
    events_by_seq: &'a BTreeMap<u64, &LoggedSpineLedgerEvent>,
) -> Result<&'a LoggedSpineLedgerEvent, SpineError> {
    events_by_seq
        .get(&marker.token_seq_start)
        .copied()
        .ok_or_else(|| {
            SpineError::InvalidStore(format!(
                "Spine commit marker {} references missing token_seq {}",
                marker.op_id, marker.token_seq_start
            ))
        })
}

fn validate_close_marker_fields(
    marker: &SpineCommitMarker,
    node: &NodeId,
    boundary: u64,
) -> Result<(), SpineError> {
    validate_marker_raw_boundary(marker, boundary, "Close")?;
    if marker.raw_live_hash.is_some() {
        return Err(SpineError::InvalidStore(format!(
            "Spine suffix commit marker {} must not carry a raw_live_hash",
            marker.op_id
        )));
    }
    let memory = single_commit_memory_ref(marker)?;
    if &memory.node != node {
        return Err(SpineError::InvalidStore(format!(
            "Spine commit marker {} memory node {} does not match Close node {}",
            marker.op_id, memory.node, node
        )));
    }
    if memory.raw_end > marker.raw_boundary {
        return Err(SpineError::InvalidStore(format!(
            "Spine commit marker {} memory raw_end {} exceeds raw boundary {}",
            marker.op_id, memory.raw_end, marker.raw_boundary
        )));
    }
    Ok(())
}

fn validate_marker_raw_live_hash_matches(
    marker: &SpineCommitMarker,
    expected: &str,
    event_label: &str,
) -> Result<(), SpineError> {
    if marker.raw_live_hash.as_deref() != Some(expected) {
        return Err(SpineError::InvalidStore(format!(
            "Spine commit marker {} raw live hash does not match {}",
            marker.op_id, event_label
        )));
    }
    Ok(())
}

fn validate_single_memory_ref_matches_root_compact(
    marker: &SpineCommitMarker,
    node: &NodeId,
    compact_id: &str,
) -> Result<(), SpineError> {
    let memory = single_commit_memory_ref(marker)?;
    if memory.compact_id.as_str() != compact_id || &memory.node != node {
        return Err(SpineError::InvalidStore(format!(
            "Spine commit marker {} memory does not match RootCompact",
            marker.op_id
        )));
    }
    Ok(())
}

fn single_commit_memory_ref(
    marker: &SpineCommitMarker,
) -> Result<&SpineCommitMemoryRef, SpineError> {
    match marker.memory_refs.as_slice() {
        [memory] => Ok(memory),
        _ => Err(SpineError::InvalidStore(format!(
            "Spine commit marker {} must reference exactly one memory artifact",
            marker.op_id
        ))),
    }
}
