use codex_protocol::spine_tree::SpineNodeContextBaselineSource;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

use super::SpineError;
use crate::spine::archive::SpineArchive;
use crate::spine::model::ContextBaselineSource;
use crate::spine::model::LoggedPressureEvent;
use crate::spine::model::LoggedSpineLedgerEvent;
use crate::spine::model::LoggedTrimEvent;
use crate::spine::model::MemRecord;
use crate::spine::model::RawMask;
use crate::spine::model::SpineCommitMarker;
use crate::spine::model::SpineLedgerEvent;
use crate::spine::model::TrimProjection;
use crate::spine::model::commit_marker_structural_event_seqs;
use crate::spine::parse_stack::ParseStack;
use crate::spine::parse_stack::apply_metadata_event;
use crate::spine::parse_stack::event_to_token;
use crate::spine::parse_stack::parse_stack_from_events_with_forced_events;
use crate::spine::trimmer::trim_projection_from_events;

pub(super) fn protocol_context_baseline_source(
    source: ContextBaselineSource,
) -> SpineNodeContextBaselineSource {
    match source {
        ContextBaselineSource::ProviderAtOpen => SpineNodeContextBaselineSource::ProviderAtOpen,
        ContextBaselineSource::RootCompactHandoff => {
            SpineNodeContextBaselineSource::RootCompactHandoff
        }
        ContextBaselineSource::EstimatedFromLiveSuffix => {
            SpineNodeContextBaselineSource::EstimatedFromLiveSuffix
        }
        ContextBaselineSource::CheckpointReplay => SpineNodeContextBaselineSource::CheckpointReplay,
    }
}

pub(super) fn live_context_baseline_source(
    source: ContextBaselineSource,
) -> Option<ContextBaselineSource> {
    match source {
        ContextBaselineSource::ProviderAtOpen | ContextBaselineSource::CheckpointReplay => {
            Some(source)
        }
        ContextBaselineSource::RootCompactHandoff
        | ContextBaselineSource::EstimatedFromLiveSuffix => None,
    }
}

pub(super) fn next_event_seq_from(events: &[LoggedSpineLedgerEvent]) -> Result<u64, SpineError> {
    events
        .iter()
        .map(|event| event.seq)
        .max()
        .map(|seq| {
            seq.checked_add(1)
                .ok_or_else(|| SpineError::InvalidEvent("spine event seq overflow".to_string()))
        })
        .transpose()
        .map(|seq| seq.unwrap_or(0))
}

pub(super) fn next_user_anchor_from_events(
    events: &[LoggedSpineLedgerEvent],
) -> Result<u64, SpineError> {
    let next = events
        .iter()
        .filter_map(|event| match &event.event {
            SpineLedgerEvent::Msg {
                user_anchor: Some(user_anchor),
                ..
            } => Some(*user_anchor),
            _ => None,
        })
        .max()
        .map(|anchor| {
            anchor
                .checked_add(1)
                .ok_or_else(|| SpineError::InvalidEvent("user anchor overflow".to_string()))
        })
        .transpose()?;
    Ok(next.unwrap_or(1))
}

pub(super) fn next_pressure_seq_from(events: &[LoggedPressureEvent]) -> Result<u64, SpineError> {
    events
        .iter()
        .map(|event| event.pressure_seq)
        .max()
        .map(|seq| {
            seq.checked_add(1)
                .ok_or_else(|| SpineError::InvalidEvent("spine pressure seq overflow".to_string()))
        })
        .transpose()
        .map(|seq| seq.unwrap_or(0))
}

pub(super) fn next_trim_seq_from(events: &[LoggedTrimEvent]) -> Result<u64, SpineError> {
    events
        .iter()
        .map(|event| event.trim_seq)
        .max()
        .map(|seq| {
            seq.checked_add(1)
                .ok_or_else(|| SpineError::InvalidEvent("spine trim seq overflow".to_string()))
        })
        .transpose()
        .map(|seq| seq.unwrap_or(0))
}

pub(crate) fn trim_projection_from_events_for_checkpoint(
    events: &[LoggedTrimEvent],
    raw_live: &[bool],
    current_structural_seq: u64,
    trim_seq_watermark: Option<u64>,
) -> Result<TrimProjection, SpineError> {
    trim_projection_from_events(events, raw_live, current_structural_seq, trim_seq_watermark)
}

pub(super) fn replay_from_events(
    archive: &SpineArchive,
    events: &[LoggedSpineLedgerEvent],
    mems: &[MemRecord],
    raw_live: &[bool],
    replay_event_seqs: &MarkerReplayEventSeqs,
    initial: Option<&ParseStack>,
    min_seq: Option<u64>,
) -> Result<ParseStack, SpineError> {
    let raw_mask = RawMask::new(raw_live);
    let Some(initial) = initial else {
        let events = events
            .iter()
            .filter(|event| min_seq.is_none_or(|min_seq| event.seq >= min_seq))
            .cloned()
            .collect::<Vec<_>>();
        return parse_stack_from_events_with_forced_events(
            &events,
            archive,
            mems,
            raw_mask,
            &replay_event_seqs.forced,
            &replay_event_seqs.marker_structural,
        );
    };
    let mem_map = mems
        .iter()
        .cloned()
        .map(|mem| (mem.compact_id.clone(), mem))
        .collect::<BTreeMap<_, _>>();
    let mut parse_stack = initial.clone();
    for event in events
        .iter()
        .filter(|event| min_seq.is_none_or(|min_seq| event.seq >= min_seq))
    {
        if matches!(event.event, SpineLedgerEvent::OpenContextBaseline { .. }) {
            continue;
        }
        if replay_event_seqs.forced.contains(&event.seq) {
            if !apply_metadata_event(&mut parse_stack, event)? {
                parse_stack.shift(event_to_token(event, archive, &mem_map, raw_mask)?, archive)?;
            }
            continue;
        }
        if replay_event_seqs.marker_structural.contains(&event.seq)
            || !event.allowed_by(raw_mask)?
        {
            continue;
        }
        if !apply_metadata_event(&mut parse_stack, event)? {
            parse_stack.shift(event_to_token(event, archive, &mem_map, raw_mask)?, archive)?;
        }
    }
    Ok(parse_stack)
}

pub(super) struct MarkerReplayEventSeqs {
    pub(super) forced: BTreeSet<u64>,
    pub(super) marker_structural: BTreeSet<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ReplayCommitClassification {
    Committed,
    Uncommitted,
}

pub(super) fn replay_event_seqs_from_markers(
    events: &[LoggedSpineLedgerEvent],
    markers: &[SpineCommitMarker],
    mems: &[MemRecord],
    raw_mask: RawMask<'_>,
    min_seq: Option<u64>,
    max_seq: Option<u64>,
    fail_on_unproved_raw_backed: bool,
) -> Result<MarkerReplayEventSeqs, SpineError> {
    let mems_by_id = mems
        .iter()
        .map(|mem| (mem.compact_id.as_str(), mem))
        .collect::<BTreeMap<_, _>>();
    let events_by_seq = events
        .iter()
        .map(|event| (event.seq, event))
        .collect::<BTreeMap<_, _>>();
    let mut forced = BTreeSet::new();
    let mut marker_structural = BTreeSet::new();
    for marker in markers {
        if !marker_in_replay_range(marker, min_seq, max_seq) {
            continue;
        }
        let structural_event_seqs = commit_marker_structural_event_seqs(marker)?;
        marker_structural.extend(structural_event_seqs.iter().copied());
        if classify_commit_marker_for_replay(
            marker,
            &structural_event_seqs,
            &events_by_seq,
            &mems_by_id,
            raw_mask,
            fail_on_unproved_raw_backed,
        )? == ReplayCommitClassification::Committed
        {
            forced.extend(structural_event_seqs);
        }
    }
    Ok(MarkerReplayEventSeqs {
        forced,
        marker_structural,
    })
}

pub(super) fn classify_commit_marker_for_replay(
    marker: &SpineCommitMarker,
    structural_event_seqs: &BTreeSet<u64>,
    events_by_seq: &BTreeMap<u64, &LoggedSpineLedgerEvent>,
    mems_by_id: &BTreeMap<&str, &MemRecord>,
    raw_mask: RawMask<'_>,
    fail_on_unproved_raw_backed: bool,
) -> Result<ReplayCommitClassification, SpineError> {
    for memory in &marker.memory_refs {
        let Some(mem) = mems_by_id.get(memory.compact_id.as_str()) else {
            return Ok(ReplayCommitClassification::Uncommitted);
        };
        if !mem.allowed_by(raw_mask)? {
            return Ok(ReplayCommitClassification::Uncommitted);
        }
    }
    for seq in marker.token_seq_start..marker.token_seq_end {
        if structural_event_seqs.contains(&seq) {
            continue;
        }
        let Some(event) = events_by_seq.get(&seq) else {
            return Ok(ReplayCommitClassification::Uncommitted);
        };
        if !event.allowed_by(raw_mask)? {
            if !fail_on_unproved_raw_backed {
                return Ok(ReplayCommitClassification::Uncommitted);
            }
            return Err(SpineError::InvalidStore(format!(
                "Spine commit marker {} raw-backed event at token_seq {} is not proved by live raw state",
                marker.op_id, seq
            )));
        }
    }
    Ok(ReplayCommitClassification::Committed)
}

fn marker_in_replay_range(
    marker: &SpineCommitMarker,
    min_seq: Option<u64>,
    max_seq: Option<u64>,
) -> bool {
    min_seq.is_none_or(|min_seq| marker.token_seq_start >= min_seq)
        && max_seq.is_none_or(|max_seq| marker.token_seq_end <= max_seq)
}
