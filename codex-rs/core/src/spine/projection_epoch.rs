use super::state::StateCheckpoint;
use codex_protocol::protocol::RolloutItem;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;
use std::collections::HashSet;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct ProjectionEpochMetadata {
    pub(crate) source_rollout_ref: String,
    pub(crate) processed_rollout_len: u64,
    pub(crate) processed_rollout_hash: String,
    pub(crate) effective_raw_len: u64,
    pub(crate) surviving_turn_ids_hash: String,
    pub(crate) surviving_compact_ids: Vec<String>,
    pub(crate) checkpoint_hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProjectionRolloutPosition {
    pub(crate) source_rollout_ref: String,
    pub(crate) processed_rollout_len: u64,
    pub(crate) processed_rollout_hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProjectionEpochClassification {
    Behind,
    Current,
    Ahead,
    Divergent,
}

pub(crate) fn projection_epoch_metadata(
    source_rollout_ref: impl Into<String>,
    rollout_items: &[RolloutItem],
    checkpoint: &StateCheckpoint,
    effective_raw_len: u64,
    surviving_turn_ids: &HashSet<String>,
    surviving_compact_ids: &HashSet<String>,
) -> Result<ProjectionEpochMetadata, ProjectionEpochError> {
    let source_rollout_ref = source_rollout_ref.into();
    let position = projection_rollout_position(source_rollout_ref, rollout_items)?;
    Ok(ProjectionEpochMetadata {
        source_rollout_ref: position.source_rollout_ref,
        processed_rollout_len: position.processed_rollout_len,
        processed_rollout_hash: position.processed_rollout_hash,
        effective_raw_len,
        surviving_turn_ids_hash: hash_sorted_strings(surviving_turn_ids),
        surviving_compact_ids: sorted_strings(surviving_compact_ids),
        checkpoint_hash: projection_checkpoint_hash(checkpoint)?,
    })
}

pub(crate) fn projection_rollout_position(
    source_rollout_ref: impl Into<String>,
    rollout_items: &[RolloutItem],
) -> Result<ProjectionRolloutPosition, ProjectionEpochError> {
    let processed_rollout_len =
        u64::try_from(rollout_items.len()).map_err(|_| ProjectionEpochError::RolloutTooLong {
            len: rollout_items.len(),
        })?;
    Ok(ProjectionRolloutPosition {
        source_rollout_ref: source_rollout_ref.into(),
        processed_rollout_len,
        processed_rollout_hash: hash_rollout_items_as_jsonl(rollout_items)?,
    })
}

pub(crate) fn classify_projection_epoch(
    epoch: &ProjectionEpochMetadata,
    current_prefix_at_epoch_len: &ProjectionRolloutPosition,
    current_processed_rollout_len: u64,
) -> ProjectionEpochClassification {
    if epoch.processed_rollout_len > current_processed_rollout_len {
        return ProjectionEpochClassification::Ahead;
    }
    if epoch.source_rollout_ref != current_prefix_at_epoch_len.source_rollout_ref {
        return ProjectionEpochClassification::Divergent;
    }
    if epoch.processed_rollout_len != current_prefix_at_epoch_len.processed_rollout_len {
        return ProjectionEpochClassification::Divergent;
    }
    if epoch.processed_rollout_hash != current_prefix_at_epoch_len.processed_rollout_hash {
        return ProjectionEpochClassification::Divergent;
    }
    if epoch.processed_rollout_len == current_processed_rollout_len {
        ProjectionEpochClassification::Current
    } else {
        ProjectionEpochClassification::Behind
    }
}

fn hash_rollout_items_as_jsonl(rollout_items: &[RolloutItem]) -> Result<String, serde_json::Error> {
    let mut hasher = Sha256::new();
    for item in rollout_items {
        let encoded = serde_json::to_string(item)?;
        hasher.update(encoded.as_bytes());
        hasher.update(b"\n");
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn projection_checkpoint_hash(checkpoint: &StateCheckpoint) -> Result<String, serde_json::Error> {
    let encoded = serde_json::to_string(checkpoint)?;
    Ok(format!("sha256:{:x}", Sha256::digest(encoded.as_bytes())))
}

fn hash_sorted_strings(values: &HashSet<String>) -> String {
    let mut joined = sorted_strings(values).join("\n");
    joined.push('\n');
    format!("sha256:{:x}", Sha256::digest(joined.as_bytes()))
}

fn sorted_strings(values: &HashSet<String>) -> Vec<String> {
    let mut values = values.iter().cloned().collect::<Vec<_>>();
    values.sort();
    values
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ProjectionEpochError {
    #[error("spine projection epoch rollout length {len} cannot fit in u64")]
    RolloutTooLong { len: usize },
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
