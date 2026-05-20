use crate::spine::state::StateCheckpoint;
use serde::Deserialize;
use serde::Serialize;

use super::SpineOperation;
use super::SpineStoreError;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum TreeEvent {
    SpineInitialized {
        seq: u64,
        initial_raw_start_ordinal: u64,
    },
    TransitionApplied {
        seq: u64,
        op: SpineOperation,
        from_node: String,
        to_node: String,
        summary: Option<String>,
        raw_start_ordinal: u64,
        source_turn_id: String,
    },
    RootEpochReset {
        seq: u64,
        root_id: String,
        next_leaf_id: String,
        summary: String,
        raw_start_ordinal: u64,
        compact_id: String,
        source_turn_id: String,
    },
    RawStartOrdinalUpdated {
        seq: u64,
        node_id: String,
        raw_start_ordinal: u64,
        source_turn_id: String,
    },
    ProjectionReset {
        seq: u64,
        reason: String,
        source_turn_id: Option<String>,
        source_rollout_ref: String,
        processed_rollout_len: u64,
        processed_rollout_hash: String,
        effective_raw_len: u64,
        surviving_turn_ids_hash: String,
        surviving_compact_ids: Vec<String>,
        checkpoint_hash: String,
        checkpoint: StateCheckpoint,
    },
    SpineHintEmitted {
        seq: u64,
        node_id: String,
        threshold_tokens: u64,
        estimated_tokens: u64,
        source: String,
    },
}

impl TreeEvent {
    pub(super) fn seq(&self) -> u64 {
        match self {
            TreeEvent::SpineInitialized { seq, .. }
            | TreeEvent::TransitionApplied { seq, .. }
            | TreeEvent::RootEpochReset { seq, .. }
            | TreeEvent::RawStartOrdinalUpdated { seq, .. }
            | TreeEvent::ProjectionReset { seq, .. }
            | TreeEvent::SpineHintEmitted { seq, .. } => *seq,
        }
    }
}

pub(super) fn next_tree_seq_for_event_count(count: usize) -> Result<u64, SpineStoreError> {
    u64::try_from(count + 1)
        .map_err(|_| SpineStoreError::InvalidLedger("tree.jsonl has too many events".into()))
}

pub(super) fn next_tree_seq_after(current_seq: u64) -> Result<u64, SpineStoreError> {
    current_seq
        .checked_add(1)
        .ok_or_else(|| SpineStoreError::InvalidLedger("tree.jsonl has too many events".into()))
}
