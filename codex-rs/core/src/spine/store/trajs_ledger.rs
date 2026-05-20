use serde::Deserialize;
use serde::Serialize;

use super::SpineOperation;
use super::jsonl_ledger::SequencedLedgerEvent;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum TrajsIndexEvent {
    RawItemsRecorded {
        seq: u64,
        node_id: String,
        turn_id: String,
        start: u64,
        end: u64,
    },
    TransitionCommitted {
        seq: u64,
        call_id: String,
        op: SpineOperation,
        from_node: String,
        to_node: String,
        call_start_ordinal: u64,
        boundary_end: u64,
    },
}

impl SequencedLedgerEvent for TrajsIndexEvent {
    fn seq(&self) -> u64 {
        match self {
            TrajsIndexEvent::RawItemsRecorded { seq, .. }
            | TrajsIndexEvent::TransitionCommitted { seq, .. } => *seq,
        }
    }

    fn set_seq(&mut self, next_seq: u64) {
        match self {
            TrajsIndexEvent::RawItemsRecorded { seq, .. }
            | TrajsIndexEvent::TransitionCommitted { seq, .. } => *seq = next_seq,
        }
    }
}
