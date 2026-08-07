use super::ReplayError;
use crate::ContextEpoch;
use crate::RawBoundary;
use crate::RecordDigest;
use crate::ThreadNamespace;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest as _;
use sha2::Sha256;

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
    pub replacement_digest: RecordDigest,
}

impl SpineCompactBarrierV1 {
    pub fn new(
        thread: ThreadNamespace,
        previous_epoch: ContextEpoch,
        next_epoch: ContextEpoch,
        boundary: RawBoundary,
        replacement_boundaries: Vec<RawBoundary>,
    ) -> Result<Self, ReplayError> {
        let mut barrier = Self {
            schema: COMPACT_BARRIER_SCHEMA_V1.to_string(),
            thread,
            previous_epoch,
            next_epoch,
            boundary,
            replacement_boundaries,
            replacement_digest: zero_digest()?,
        };
        barrier.replacement_digest = barrier.computed_digest()?;
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
        if self.replacement_digest != self.computed_digest()? {
            return Err(ReplayError::InvalidCompactBarrier(
                "replacement digest does not match",
            ));
        }
        Ok(())
    }

    fn computed_digest(&self) -> Result<RecordDigest, ReplayError> {
        let mut canonical = self.clone();
        canonical.replacement_digest = zero_digest()?;
        let encoded = serde_json::to_vec(&canonical)
            .map_err(|error| ReplayError::Serialize(error.to_string()))?;
        RecordDigest::parse(format!("{:x}", Sha256::digest(encoded)))
            .map_err(|error| ReplayError::Serialize(error.to_string()))
    }
}

fn zero_digest() -> Result<RecordDigest, ReplayError> {
    RecordDigest::parse("0".repeat(64)).map_err(|error| ReplayError::Serialize(error.to_string()))
}
