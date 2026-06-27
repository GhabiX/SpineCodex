use super::super::SpineCloneBoundary;
use super::super::commit_marker;
use crate::spine::SpineError;
use crate::spine::model::LoggedSpineLedgerEvent;
use crate::spine::model::RawMask;
use crate::spine::model::SpineCommitMarker;
use crate::spine::model::commit_marker_structural_event_seqs;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

pub(in crate::spine::store::clone_sidecar) fn select_cloned_commit_markers(
    source_commit_markers: Vec<SpineCommitMarker>,
    source_events_by_seq: &BTreeMap<u64, &LoggedSpineLedgerEvent>,
    boundary: &SpineCloneBoundary,
    source_raw_live: &[bool],
    mask: RawMask<'_>,
) -> Result<(Vec<SpineCommitMarker>, BTreeSet<u64>), SpineError> {
    let mut all_marker_structural_event_seqs = BTreeSet::new();
    let mut cloned_commit_markers = Vec::new();
    for marker in source_commit_markers {
        commit_marker::validate_commit_marker_record(&marker)?;
        commit_marker::validate_commit_marker_events(&marker, source_events_by_seq)?;
        let structural_event_seqs = commit_marker_structural_event_seqs(&marker)?;
        all_marker_structural_event_seqs.extend(structural_event_seqs.iter().copied());
        if !commit_marker_within_clone_boundary(&marker, boundary) {
            continue;
        }
        if !commit_marker::commit_marker_allowed_by_source_live(&marker, source_raw_live)? {
            return Err(SpineError::InvalidStore(format!(
                "Spine commit marker {} is not proved by clone raw live state",
                marker.op_id
            )));
        }
        validate_raw_backed_marker_events(
            &marker,
            &structural_event_seqs,
            source_events_by_seq,
            mask,
        )?;
        cloned_commit_markers.push(marker);
    }
    Ok((cloned_commit_markers, all_marker_structural_event_seqs))
}

fn commit_marker_within_clone_boundary(
    marker: &SpineCommitMarker,
    boundary: &SpineCloneBoundary,
) -> bool {
    marker.token_seq_end <= boundary.structural_seq_limit
        && marker.raw_boundary <= boundary.raw_ordinal_limit
}

fn validate_raw_backed_marker_events(
    marker: &SpineCommitMarker,
    structural_event_seqs: &BTreeSet<u64>,
    source_events_by_seq: &BTreeMap<u64, &LoggedSpineLedgerEvent>,
    mask: RawMask<'_>,
) -> Result<(), SpineError> {
    for seq in (marker.token_seq_start..marker.token_seq_end)
        .filter(|seq| !structural_event_seqs.contains(seq))
    {
        let Some(event) = source_events_by_seq.get(&seq) else {
            return Err(SpineError::InvalidStore(format!(
                "Spine commit marker {} references missing raw-backed event at token_seq {}",
                marker.op_id, seq
            )));
        };
        if !event.allowed_by(mask)? {
            return Err(SpineError::InvalidStore(format!(
                "Spine commit marker {} raw-backed event at token_seq {} is not proved by clone raw live state",
                marker.op_id, seq
            )));
        }
    }
    Ok(())
}

pub(in crate::spine::store::clone_sidecar) fn select_cloned_events(
    source_events: Vec<LoggedSpineLedgerEvent>,
    cloned_commit_markers: &[SpineCommitMarker],
    all_marker_structural_event_seqs: &BTreeSet<u64>,
    boundary: &SpineCloneBoundary,
    mask: RawMask<'_>,
) -> Result<Vec<LoggedSpineLedgerEvent>, SpineError> {
    let mut marker_proved_event_seqs = BTreeSet::new();
    for marker in cloned_commit_markers {
        marker_proved_event_seqs.extend(commit_marker_structural_event_seqs(marker)?);
    }
    let mut cloned_events = Vec::new();
    for event in source_events {
        if event.seq >= boundary.structural_seq_limit {
            continue;
        }
        if marker_proved_event_seqs.contains(&event.seq)
            || (!all_marker_structural_event_seqs.contains(&event.seq) && event.allowed_by(mask)?)
        {
            cloned_events.push(event);
        }
    }
    Ok(cloned_events)
}
