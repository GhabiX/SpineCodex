use serde::Deserialize;
use serde::Serialize;

use super::jsonl_ledger::SequencedLedgerEvent;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum HintEvent {
    SizeHintEmitted {
        seq: u64,
        node_id: String,
        threshold_tokens: u64,
        estimated_tokens: u64,
        source: String,
    },
}

impl SequencedLedgerEvent for HintEvent {
    fn seq(&self) -> u64 {
        match self {
            HintEvent::SizeHintEmitted { seq, .. } => *seq,
        }
    }

    fn set_seq(&mut self, next_seq: u64) {
        match self {
            HintEvent::SizeHintEmitted { seq, .. } => *seq = next_seq,
        }
    }
}
