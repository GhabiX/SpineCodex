use super::ReplayError;
use crate::ContextEpoch;
use crate::RawBoundary;
use crate::RecordDigest;
use crate::ThreadNamespace;
use serde::Deserialize;
use serde::Serialize;

pub const COMPACT_BARRIER_SCHEMA_V1: &str = "spine.compact.barrier.v1";
pub const MAX_COMPACT_REPLACEMENT_BYTES: usize = crate::MAX_RAW_EVENT_BYTES;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpineCompactBarrierV1 {
    pub schema: String,
    pub thread: ThreadNamespace,
    pub previous_epoch: ContextEpoch,
    pub next_epoch: ContextEpoch,
    pub boundary: RawBoundary,
    pub replacement_boundaries: Vec<RawBoundary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replacement_digest: Option<RecordDigest>,
}

impl SpineCompactBarrierV1 {
    pub fn new(
        thread: ThreadNamespace,
        previous_epoch: ContextEpoch,
        next_epoch: ContextEpoch,
        boundary: RawBoundary,
        replacement_boundaries: Vec<RawBoundary>,
    ) -> Result<Self, ReplayError> {
        let barrier = Self {
            schema: COMPACT_BARRIER_SCHEMA_V1.to_string(),
            thread,
            previous_epoch,
            next_epoch,
            boundary,
            replacement_boundaries,
            replacement_digest: None,
        };
        barrier.validate()?;
        Ok(barrier)
    }

    pub fn validate(&self) -> Result<(), ReplayError> {
        if self.schema != COMPACT_BARRIER_SCHEMA_V1 {
            return Err(ReplayError::InvalidCompactBarrier("unsupported schema"));
        }
        if self.previous_epoch.checked_next() != Some(self.next_epoch) {
            return Err(ReplayError::InvalidCompactBarrier(
                "compact epoch must advance exactly once",
            ));
        }
        if self
            .replacement_boundaries
            .windows(2)
            .any(|boundaries| boundaries[0] >= boundaries[1])
            || self
                .replacement_boundaries
                .first()
                .is_some_and(|boundary| *boundary <= self.boundary)
        {
            return Err(ReplayError::InvalidCompactBarrier(
                "replacement boundaries must be ordered after compact",
            ));
        }
        let encoded = serde_json::to_vec(&self.replacement_boundaries)
            .map_err(|error| ReplayError::Serialize(error.to_string()))?;
        if encoded.len() > MAX_COMPACT_REPLACEMENT_BYTES {
            return Err(ReplayError::InvalidCompactBarrier(
                "replacement history exceeds the bounded payload",
            ));
        }
        Ok(())
    }
}
