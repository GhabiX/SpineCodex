use super::CodexContextPlanError;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::SpineSamplingStartedItem;
use codex_protocol::protocol::SpineTransitionItem;
use spine_core::host::PlannerError;
use spine_core::host::SamplingArchiveRecord;
use spine_core::host::ThreadNamespace;
use thiserror::Error;

const SPINE_ROLLOUT_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ReplayMode {
    Native,
    Canonical {
        thread: ThreadNamespace,
        records: Vec<SamplingArchiveRecord>,
    },
}

fn encode_record(record: &SamplingArchiveRecord) -> Result<serde_json::Value, CoordinatorError> {
    let encoded = record
        .encode()
        .map_err(|error| CoordinatorError::Archive(error.to_string()))?;
    let payload = serde_json::from_slice(&encoded)
        .map_err(|error| CoordinatorError::Codec(error.to_string()))?;
    Ok(payload)
}

pub(crate) fn encode_spine_sampling_started(
    record: &SamplingArchiveRecord,
) -> Result<SpineSamplingStartedItem, CoordinatorError> {
    if !matches!(record, SamplingArchiveRecord::SamplingStarted(_)) {
        return Err(CoordinatorError::Archive(
            "Spine sampling-start item requires a sampling-started record".to_string(),
        ));
    }
    Ok(SpineSamplingStartedItem {
        version: SPINE_ROLLOUT_VERSION,
        payload: encode_record(record)?,
    })
}

pub(crate) fn encode_spine_transition(
    record: &SamplingArchiveRecord,
) -> Result<SpineTransitionItem, CoordinatorError> {
    if !matches!(record, SamplingArchiveRecord::SamplingCommit(_)) {
        return Err(CoordinatorError::Archive(
            "Spine transition item requires a sampling commit".to_string(),
        ));
    }
    Ok(SpineTransitionItem {
        version: SPINE_ROLLOUT_VERSION,
        payload: encode_record(record)?,
    })
}

fn decode_record(
    version: u32,
    payload: &serde_json::Value,
) -> Result<SamplingArchiveRecord, CoordinatorError> {
    if version != SPINE_ROLLOUT_VERSION {
        return Err(CoordinatorError::UnsupportedVersion(version));
    }
    let encoded =
        serde_json::to_vec(payload).map_err(|error| CoordinatorError::Codec(error.to_string()))?;
    SamplingArchiveRecord::decode(&encoded)
        .map_err(|error| CoordinatorError::Archive(error.to_string()))
}

pub(crate) fn decode_spine_rollout_item(
    item: &RolloutItem,
) -> Result<Option<SamplingArchiveRecord>, CoordinatorError> {
    let record = match item {
        RolloutItem::SpineSamplingStarted(item) => decode_record(item.version, &item.payload)?,
        RolloutItem::SpineTransition(item) => decode_record(item.version, &item.payload)?,
        _ => return Ok(None),
    };
    match (item, &record) {
        (RolloutItem::SpineSamplingStarted(_), SamplingArchiveRecord::SamplingStarted(_))
        | (RolloutItem::SpineTransition(_), SamplingArchiveRecord::SamplingCommit(_)) => {
            Ok(Some(record))
        }
        _ => Err(CoordinatorError::Archive(
            "Spine rollout item kind does not match its archive record".to_string(),
        )),
    }
}

pub(crate) fn replay_mode(
    effective: &[(usize, &RolloutItem)],
) -> Result<ReplayMode, CoordinatorError> {
    let mut canonical_thread = None;
    let mut canonical_commit = false;
    let mut records = Vec::new();
    for (_, item) in effective {
        let Some(record) = decode_spine_rollout_item(item)? else {
            continue;
        };
        match &record {
            SamplingArchiveRecord::SamplingStarted(started) => {
                canonical_thread.get_or_insert_with(|| started.attempt_id.thread().clone());
            }
            SamplingArchiveRecord::SamplingCommit(_) => canonical_commit = true,
        }
        records.push(record);
    }
    if canonical_commit && canonical_thread.is_none() {
        return Err(CoordinatorError::Replay(
            "canonical sampling commit has no matching sampling-started record".to_string(),
        ));
    }
    Ok(match canonical_thread {
        Some(thread) => ReplayMode::Canonical { thread, records },
        None => ReplayMode::Native,
    })
}

#[derive(Debug, Error)]
pub(crate) enum CoordinatorError {
    #[error("invalid Spine identity: {0}")]
    Identity(String),
    #[error("Spine planner failed: {0}")]
    Planner(#[from] PlannerError),
    #[error("Spine sampling failed: {0}")]
    Sampling(#[from] spine_core::host::SamplingError),
    #[error("Spine archive failed: {0}")]
    Archive(String),
    #[error("Spine rollout codec failed: {0}")]
    Codec(String),
    #[error("Spine context plan failed: {0}")]
    ContextPlan(#[from] CodexContextPlanError),
    #[error("Spine replay failed: {0}")]
    Replay(String),
    #[error("unsupported Spine rollout version {0}")]
    UnsupportedVersion(u32),
    #[error("Spine durability is faulted: {0}")]
    DurabilityFaulted(String),
}
