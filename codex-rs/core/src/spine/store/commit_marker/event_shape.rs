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
    let shape = commit_marker_event_shape(marker.kind);
    validate_commit_marker_width(marker, shape.width())?;
    match shape {
        CommitMarkerEventShape::Close => {
            let (node, boundary) = close_event_at_marker_start(marker, events_by_seq)?;
            validate_close_marker_fields(marker, node, *boundary)?;
            validate_required_trailing_toolcall(
                marker,
                events_by_seq,
                marker_shape_seq(marker, shape.trailing_toolcall_offset())?,
            )?;
            Ok(())
        }
        CommitMarkerEventShape::CloseThenOpen => {
            validate_close_prefix(marker, events_by_seq)?;
            validate_required_synthetic_open(
                marker,
                events_by_seq,
                marker_shape_seq(marker, shape.synthetic_open_offset())?,
            )?;
            validate_required_trailing_toolcall(
                marker,
                events_by_seq,
                marker_shape_seq(marker, shape.trailing_toolcall_offset())?,
            )?;
            Ok(())
        }
        CommitMarkerEventShape::RootCompact => {
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
            if *boundary != marker.raw_boundary {
                return Err(SpineError::InvalidStore(format!(
                    "Spine commit marker {} raw boundary {} does not match RootCompact boundary {}",
                    marker.op_id, marker.raw_boundary, boundary
                )));
            }
            if marker.raw_live_hash.as_deref() != Some(raw_live_hash.as_str()) {
                return Err(SpineError::InvalidStore(format!(
                    "Spine commit marker {} raw live hash does not match RootCompact",
                    marker.op_id
                )));
            }
            let memory = single_commit_memory_ref(marker)?;
            if &memory.compact_id != mem || &memory.node != node {
                return Err(SpineError::InvalidStore(format!(
                    "Spine commit marker {} memory does not match RootCompact",
                    marker.op_id
                )));
            }
            Ok(())
        }
    }
}

fn validate_close_prefix(
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
    if *boundary != marker.raw_boundary {
        return Err(SpineError::InvalidStore(format!(
            "Spine commit marker {} raw boundary {} does not match synthetic Open boundary {}",
            marker.op_id, marker.raw_boundary, boundary
        )));
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum CommitMarkerEventShape {
    Close,
    CloseThenOpen,
    RootCompact,
}

impl CommitMarkerEventShape {
    fn width(self) -> u64 {
        match self {
            Self::Close => 2,
            Self::CloseThenOpen => 3,
            Self::RootCompact => 1,
        }
    }

    fn synthetic_open_offset(self) -> u64 {
        match self {
            Self::CloseThenOpen => 1,
            Self::Close | Self::RootCompact => 0,
        }
    }

    fn trailing_toolcall_offset(self) -> u64 {
        match self {
            Self::Close => 1,
            Self::CloseThenOpen => 2,
            Self::RootCompact => 0,
        }
    }
}

fn commit_marker_event_shape(kind: SpineCommitKindMarker) -> CommitMarkerEventShape {
    match kind {
        SpineCommitKindMarker::Close => CommitMarkerEventShape::Close,
        SpineCommitKindMarker::CloseThenOpen => CommitMarkerEventShape::CloseThenOpen,
        SpineCommitKindMarker::RootCompact => CommitMarkerEventShape::RootCompact,
    }
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
    let expected_end = marker.token_seq_start.checked_add(width).ok_or_else(|| {
        SpineError::InvalidEvent("Spine commit marker token seq overflow".to_string())
    })?;
    if marker.token_seq_end != expected_end {
        return Err(SpineError::InvalidStore(format!(
            "Spine commit marker {} token range {}..{} has unexpected width {width}",
            marker.op_id, marker.token_seq_start, marker.token_seq_end
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
    if boundary != marker.raw_boundary {
        return Err(SpineError::InvalidStore(format!(
            "Spine commit marker {} raw boundary {} does not match Close boundary {}",
            marker.op_id, marker.raw_boundary, boundary
        )));
    }
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
