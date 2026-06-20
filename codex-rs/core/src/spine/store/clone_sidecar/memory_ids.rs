use crate::spine::SpineError;
use crate::spine::model::LoggedSpineLedgerEvent;
use crate::spine::model::MemRecord;
use crate::spine::model::RawMask;
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
                let mut candidates = mems
                    .iter()
                    .filter(|mem| &mem.node == node)
                    .collect::<Vec<_>>();
                candidates.sort_by(|left, right| left.compact_id.cmp(&right.compact_id));
                let mut selected = None;
                for mem in candidates {
                    if mem.allowed_by(raw_mask)? {
                        selected = Some(mem);
                        break;
                    }
                }
                let mem = selected.ok_or_else(|| {
                    SpineError::InvalidEvent(format!("missing memory for close node {node}"))
                })?;
                ids.insert(mem.compact_id.clone());
            }
            SpineLedgerEvent::RootCompact { mem, .. } => {
                let mem_record = mems
                    .iter()
                    .find(|record| record.compact_id == *mem)
                    .ok_or_else(|| {
                        SpineError::InvalidEvent("missing memory for root compact".to_string())
                    })?;
                if !mem_record.allowed_by(raw_mask)? {
                    return Err(SpineError::InvalidEvent(format!(
                        "memory {} does not cover live raw evidence",
                        mem_record.compact_id
                    )));
                }
                ids.insert(mem.clone());
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
