use crate::spine::SpineError;
use crate::spine::checkpoint::SpineCheckpoint;
use crate::spine::compact_checkpoint::SpineCompactCheckpoint;
use crate::spine::model::LoggedSpineLedgerEvent;
use crate::spine::model::MemRecord;
use crate::spine::model::RawMask;
use crate::spine::model::SpineCommitMarker;
use crate::spine::model::SpineLedgerEvent;
use std::collections::BTreeSet;

pub(in crate::spine::store::clone_sidecar) fn required_memory_ids_for_cloned_events(
    events: &[LoggedSpineLedgerEvent],
    mems: &[MemRecord],
    raw_mask: RawMask<'_>,
) -> Result<BTreeSet<String>, SpineError> {
    let mut ids = BTreeSet::new();
    for event in events {
        match &event.event {
            SpineLedgerEvent::Close { node, .. } => {
                let mem = required_close_memory(node, mems, raw_mask)?;
                ids.insert(mem.compact_id.clone());
            }
            SpineLedgerEvent::RootCompact { mem, .. } => {
                let mem = required_root_compact_memory(mem, mems, raw_mask)?;
                ids.insert(mem.compact_id.clone());
            }
            SpineLedgerEvent::Init { .. }
            | SpineLedgerEvent::Msg { .. }
            | SpineLedgerEvent::ToolCall { .. }
            | SpineLedgerEvent::Open { .. }
            | SpineLedgerEvent::OpenContextBaseline { .. } => {}
        }
    }
    Ok(ids)
}

fn required_close_memory<'a>(
    node: &crate::spine::model::NodeId,
    mems: &'a [MemRecord],
    raw_mask: RawMask<'_>,
) -> Result<&'a MemRecord, SpineError> {
    let mut candidates = mems
        .iter()
        .filter(|mem| &mem.node == node)
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| left.compact_id.cmp(&right.compact_id));
    for mem in candidates {
        if mem.allowed_by(raw_mask)? {
            return Ok(mem);
        }
    }
    Err(SpineError::InvalidEvent(format!(
        "missing memory for close node {node}"
    )))
}

fn required_root_compact_memory<'a>(
    compact_id: &str,
    mems: &'a [MemRecord],
    raw_mask: RawMask<'_>,
) -> Result<&'a MemRecord, SpineError> {
    let mem_record = mems
        .iter()
        .find(|record| record.compact_id == compact_id)
        .ok_or_else(|| SpineError::InvalidEvent("missing memory for root compact".to_string()))?;
    if !mem_record.allowed_by(raw_mask)? {
        return Err(SpineError::InvalidEvent(format!(
            "memory {} does not cover live raw evidence",
            mem_record.compact_id
        )));
    }
    Ok(mem_record)
}

pub(in crate::spine::store::clone_sidecar) fn add_required_memory_refs(
    ids: &mut BTreeSet<String>,
    compact_checkpoints: &[SpineCompactCheckpoint],
    checkpoints: &[SpineCheckpoint],
    commit_markers: &[SpineCommitMarker],
) {
    ids.extend(
        compact_checkpoints
            .iter()
            .flat_map(|checkpoint| &checkpoint.memory_refs)
            .map(|memory| memory.compact_id.clone()),
    );
    ids.extend(
        checkpoints
            .iter()
            .flat_map(|checkpoint| &checkpoint.memory_refs)
            .map(|memory| memory.compact_id.clone()),
    );
    ids.extend(
        commit_markers
            .iter()
            .flat_map(|marker| &marker.memory_refs)
            .map(|memory| memory.compact_id.clone()),
    );
}
