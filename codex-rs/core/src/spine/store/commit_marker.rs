use super::sidecar_store_path;
use crate::spine::SpineError;
use crate::spine::io::hash_raw_live;
use crate::spine::io::sha1_hex;
use crate::spine::model::COMMIT_MARKER_VERSION;
use crate::spine::model::LoggedSpineLedgerEvent;
use crate::spine::model::MemKind;
use crate::spine::model::MemRecord;
use crate::spine::model::NodeId;
use crate::spine::model::RawMask;
use crate::spine::model::SpineCommitKindMarker;
use crate::spine::model::SpineCommitMarker;
use crate::spine::model::SpineCommitMemoryRef;
use crate::spine::model::SpineLedgerEvent;
use std::collections::BTreeMap;
use std::path::Path;

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
        validate_commit_marker_memory_refs(store_root, marker, mems, raw_live)?;
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

pub(super) fn validate_commit_marker_events(
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
            let (node, boundary) = close_event_at_marker_start(marker, events_by_seq)?;
            validate_close_marker_fields(marker, node, *boundary)?;
            let open_seq = marker_shape_seq(marker, shape.synthetic_open_offset())?;
            let Some(open) = events_by_seq.get(&open_seq) else {
                return Err(SpineError::InvalidStore(format!(
                    "Spine commit marker {} is missing synthetic Open at token_seq {}",
                    marker.op_id, open_seq
                )));
            };
            let SpineLedgerEvent::Open { boundary, .. } = &open.event else {
                return Err(SpineError::InvalidStore(format!(
                    "Spine commit marker {} is not followed by synthetic Open at token_seq {}",
                    marker.op_id, open_seq
                )));
            };
            if *boundary != marker.raw_boundary {
                return Err(SpineError::InvalidStore(format!(
                    "Spine commit marker {} raw boundary {} does not match synthetic Open boundary {}",
                    marker.op_id, marker.raw_boundary, boundary
                )));
            }
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

pub(super) fn commit_marker_allowed_by_source_live(
    marker: &SpineCommitMarker,
    raw_live: &[bool],
) -> Result<bool, SpineError> {
    if marker.raw_boundary
        > u64::try_from(raw_live.len())
            .map_err(|_| SpineError::InvalidEvent("raw live length overflow".to_string()))?
    {
        return Ok(false);
    }
    if let Some(raw_live_hash) = marker.raw_live_hash.as_deref()
        && !raw_live_prefix_hash_matches(raw_live, marker.raw_boundary, raw_live_hash)?
    {
        return Ok(false);
    }
    for memory in &marker.memory_refs {
        if !commit_memory_ref_allowed_by_source_live(memory, raw_live)? {
            return Ok(false);
        }
    }
    Ok(true)
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

fn validate_commit_marker_memory_refs(
    store_root: &Path,
    marker: &SpineCommitMarker,
    mems: &[MemRecord],
    raw_live: &[bool],
) -> Result<(), SpineError> {
    for memory in &marker.memory_refs {
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
        if memory.kind != mem.kind
            || memory.node != mem.node
            || memory.raw_start != mem.raw_start
            || memory.raw_end != mem.raw_end
            || memory.context_start != mem.context_start
            || memory.context_end != mem.context_end
            || memory.raw_live_hash != mem.raw_live_hash
            || memory.body_path != mem.body_path
            || memory.body_hash != mem.body_hash
        {
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
        let body = std::fs::read_to_string(&body_path)?;
        if sha1_hex(body.as_bytes()) != memory.body_hash {
            return Err(SpineError::InvalidStore(format!(
                "memory body hash mismatch for {}",
                memory.compact_id
            )));
        }
    }
    if let Some(raw_live_hash) = marker.raw_live_hash.as_deref()
        && !raw_live_prefix_hash_matches(raw_live, marker.raw_boundary, raw_live_hash)?
    {
        return Err(SpineError::InvalidStore(format!(
            "Spine commit marker {} raw boundary {} is not proved by durable raw live state",
            marker.op_id, marker.raw_boundary
        )));
    }
    Ok(())
}

fn commit_memory_ref_allowed_by_source_live(
    memory: &SpineCommitMemoryRef,
    raw_live: &[bool],
) -> Result<bool, SpineError> {
    match memory.kind {
        MemKind::Suffix => {
            let start = usize::try_from(memory.raw_start)
                .map_err(|_| SpineError::InvalidEvent("raw start overflow".to_string()))?;
            let end = usize::try_from(memory.raw_end)
                .map_err(|_| SpineError::InvalidEvent("raw end overflow".to_string()))?;
            Ok(start <= end
                && end <= raw_live.len()
                && raw_live[start..end].iter().all(|live| *live))
        }
        MemKind::RootEpoch => memory
            .raw_live_hash
            .as_deref()
            .map(|hash| raw_live_prefix_hash_matches(raw_live, memory.raw_end, hash))
            .unwrap_or(Ok(false)),
    }
}

fn raw_live_prefix_hash_matches(
    raw_live: &[bool],
    boundary: u64,
    expected: &str,
) -> Result<bool, SpineError> {
    let boundary = usize::try_from(boundary)
        .map_err(|_| SpineError::InvalidEvent("raw boundary overflow".to_string()))?;
    if boundary > raw_live.len() {
        return Ok(false);
    }
    Ok(hash_raw_live(&raw_live[..boundary]) == expected)
}
