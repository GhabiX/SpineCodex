use crate::ExecutionId;
use crate::context_plan::ContextPlanError;
use crate::executed_fact::ExecutionOrigin;
use crate::executed_fact::SpineOperationFact;
use crate::identity::AdmissionOrdinal;
use crate::identity::BoundaryId;
use crate::identity::ContextEpoch;
use crate::identity::SamplingAttemptId;
use crate::identity::SamplingCommitId;
use crate::identity::SourceCellId;
use crate::identity::ThreadNamespace;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeSet;
use thiserror::Error;

pub const SAMPLING_STARTED_SCHEMA: &str = "spine.sampling.started";
pub const SAMPLING_COMMIT_SCHEMA: &str = "spine.sampling.commit";
pub const MAX_ARCHIVE_RECORD_BYTES: usize = 256 * 1024;
pub const MAX_FACTS_PER_SAMPLING: usize = 64;
pub const DIGEST_HEX_BYTES: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RecordDigest(String);

impl RecordDigest {
    pub fn parse(value: impl Into<String>) -> Result<Self, ArchiveError> {
        let value = value.into();
        if value.len() != DIGEST_HEX_BYTES
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err(ArchiveError::InvalidDigest);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for RecordDigest {
    type Error = ArchiveError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<RecordDigest> for String {
    fn from(value: RecordDigest) -> Self {
        value.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SamplingStarted {
    pub schema: String,
    pub attempt_id: SamplingAttemptId,
    pub epoch: ContextEpoch,
    pub pre_boundary: BoundaryId,
    pub previous_commit_id: Option<SamplingCommitId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_digest: Option<RecordDigest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_digest: Option<RecordDigest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_digest: Option<RecordDigest>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommittedSpineExecution {
    pub execution_id: ExecutionId,
    pub ordinal: AdmissionOrdinal,
    pub origin: ExecutionOrigin,
    pub source_span: SourceSpan,
    pub operation: SpineOperationFact,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceSpan {
    pub start: SourceCellId,
    pub end: SourceCellId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SamplingCommit {
    pub schema: String,
    pub attempt_id: SamplingAttemptId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_record_digest: Option<RecordDigest>,
    pub commit_id: SamplingCommitId,
    pub epoch: ContextEpoch,
    pub previous_pre_boundary: Option<BoundaryId>,
    pub pre_boundary: BoundaryId,
    pub post_boundary: BoundaryId,
    pub previous_commit_id: Option<SamplingCommitId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    pub executions: Vec<CommittedSpineExecution>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_digest: Option<RecordDigest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_digest: Option<RecordDigest>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FactSourceBinding {
    pub execution_id: ExecutionId,
    pub start: SourceCellId,
    pub end: SourceCellId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "record",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum SamplingArchiveRecord {
    SamplingStarted(SamplingStarted),
    SamplingCommit(SamplingCommit),
}

impl SamplingArchiveRecord {
    pub fn decode(encoded: &[u8]) -> Result<Self, ArchiveError> {
        if encoded.len() > MAX_ARCHIVE_RECORD_BYTES {
            return Err(ArchiveError::RecordTooLarge {
                max_bytes: MAX_ARCHIVE_RECORD_BYTES,
                actual_bytes: encoded.len(),
            });
        }
        let record: Self = serde_json::from_slice(encoded).map_err(ArchiveError::Deserialize)?;
        record.validate()?;
        Ok(record)
    }

    pub fn encode(&self) -> Result<Vec<u8>, ArchiveError> {
        self.validate()?;
        serde_json::to_vec(self).map_err(ArchiveError::Serialize)
    }

    pub fn finalize(self) -> Result<Self, ArchiveError> {
        self.validate()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), ArchiveError> {
        self.validate_structure()
    }

    fn validate_structure(&self) -> Result<(), ArchiveError> {
        match self {
            Self::SamplingStarted(record) => {
                require_schema(&record.schema, SAMPLING_STARTED_SCHEMA)?;
                require_attempt_scope(&record.attempt_id, record.epoch, &record.pre_boundary)?;
            }
            Self::SamplingCommit(record) => {
                require_schema(&record.schema, SAMPLING_COMMIT_SCHEMA)?;
                validate_sampling_commit(
                    &record.attempt_id,
                    &record.commit_id,
                    record.epoch,
                    record.previous_pre_boundary.as_ref(),
                    &record.pre_boundary,
                    &record.post_boundary,
                    record.previous_commit_id.as_ref(),
                    &record.executions,
                )?;
            }
        }

        let encoded = serde_json::to_vec(self).map_err(ArchiveError::Serialize)?;
        if encoded.len() > MAX_ARCHIVE_RECORD_BYTES {
            return Err(ArchiveError::RecordTooLarge {
                max_bytes: MAX_ARCHIVE_RECORD_BYTES,
                actual_bytes: encoded.len(),
            });
        }
        Ok(())
    }

    pub fn commit_id(&self) -> Option<&SamplingCommitId> {
        match self {
            Self::SamplingCommit(record) => Some(&record.commit_id),
            Self::SamplingStarted(_) => None,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_sampling_commit(
    attempt_id: &SamplingAttemptId,
    commit_id: &SamplingCommitId,
    epoch: ContextEpoch,
    previous_pre_boundary: Option<&BoundaryId>,
    pre_boundary: &BoundaryId,
    post_boundary: &BoundaryId,
    previous_commit_id: Option<&SamplingCommitId>,
    executions: &[CommittedSpineExecution],
) -> Result<(), ArchiveError> {
    require_attempt_scope(attempt_id, epoch, pre_boundary)?;
    require_boundary_scope(attempt_id.thread(), epoch, post_boundary)?;
    if let Some(previous) = previous_pre_boundary {
        require_boundary_scope(attempt_id.thread(), epoch, previous)?;
        if previous.ordinal() > pre_boundary.ordinal() {
            return Err(ArchiveError::InvalidBoundaryOrder);
        }
    }
    if pre_boundary.ordinal() > post_boundary.ordinal() {
        return Err(ArchiveError::InvalidBoundaryOrder);
    }
    if commit_id.thread() != attempt_id.thread() {
        return Err(ArchiveError::IdentityScopeMismatch);
    }
    if previous_commit_id == Some(commit_id) {
        return Err(ArchiveError::InvalidCommitChain);
    }
    if previous_pre_boundary.is_some() {
        require_optional_commit_scope(attempt_id.thread(), previous_commit_id)?;
    }
    if executions.len() > MAX_FACTS_PER_SAMPLING {
        return Err(ArchiveError::TooManyFacts {
            max: MAX_FACTS_PER_SAMPLING,
            actual: executions.len(),
        });
    }
    let mut previous_ordinal = None;
    let mut seen_executions: BTreeSet<ExecutionId> = BTreeSet::new();
    let mut structural_count = 0usize;
    for execution in executions {
        let fact = crate::executed_fact::ExecutedSpineFact {
            execution_id: execution.execution_id.clone(),
            ordinal: execution.ordinal,
            origin: execution.origin.clone(),
            operation: execution.operation.clone(),
        };
        fact.validate().map_err(ArchiveError::InvalidFact)?;
        if execution.execution_id.thread() != attempt_id.thread() {
            return Err(ArchiveError::IdentityScopeMismatch);
        }
        let source = &execution.source_span;
        if source.start.thread() != attempt_id.thread()
            || source.end.thread() != attempt_id.thread()
            || source.start.epoch() != epoch
            || source.end.epoch() != epoch
            || source.start.ordinal() > source.end.ordinal()
        {
            return Err(ArchiveError::InvalidFactSourceBinding);
        }
        if previous_ordinal.is_some_and(|previous| fact.ordinal <= previous) {
            return Err(ArchiveError::InvalidFactOrder);
        }
        previous_ordinal = Some(fact.ordinal);
        if !seen_executions.insert(fact.execution_id.clone()) {
            return Err(ArchiveError::ConflictingFacts);
        }
        match fact.operation {
            SpineOperationFact::Open { .. }
            | SpineOperationFact::Close { .. }
            | SpineOperationFact::Next { .. }
            | SpineOperationFact::Spawn { .. } => {
                structural_count = structural_count.saturating_add(1);
            }
        }
    }
    if structural_count > 1 {
        return Err(ArchiveError::ConflictingFacts);
    }
    Ok(())
}

fn require_schema(actual: &str, expected: &'static str) -> Result<(), ArchiveError> {
    if actual == expected {
        Ok(())
    } else {
        Err(ArchiveError::UnsupportedSchema {
            expected,
            actual: actual.to_string(),
        })
    }
}

fn require_attempt_scope(
    attempt: &SamplingAttemptId,
    epoch: ContextEpoch,
    boundary: &BoundaryId,
) -> Result<(), ArchiveError> {
    require_boundary_scope(attempt.thread(), epoch, boundary)
}

fn require_boundary_scope(
    thread: &ThreadNamespace,
    epoch: ContextEpoch,
    boundary: &BoundaryId,
) -> Result<(), ArchiveError> {
    if boundary.thread() != thread || boundary.epoch() != epoch {
        return Err(ArchiveError::IdentityScopeMismatch);
    }
    Ok(())
}

fn require_optional_commit_scope(
    thread: &ThreadNamespace,
    commit: Option<&SamplingCommitId>,
) -> Result<(), ArchiveError> {
    if commit.is_some_and(|commit| commit.thread() != thread) {
        return Err(ArchiveError::IdentityScopeMismatch);
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum ArchiveError {
    #[error("record digest must be {DIGEST_HEX_BYTES} lowercase hex bytes")]
    InvalidDigest,
    #[error("sampling archive digest mismatch: expected {}, got {}", expected.as_str(), actual.as_str())]
    DigestMismatch {
        expected: RecordDigest,
        actual: RecordDigest,
    },
    #[error("unsupported archive schema `{actual}`; expected `{expected}`")]
    UnsupportedSchema {
        expected: &'static str,
        actual: String,
    },
    #[error("archive identities do not share a thread and epoch")]
    IdentityScopeMismatch,
    #[error("archive boundaries are not monotonic")]
    InvalidBoundaryOrder,
    #[error("sampling facts are not in strict admission order")]
    InvalidFactOrder,
    #[error("sampling commit contains conflicting facts")]
    ConflictingFacts,
    #[error("sampling commit cannot name itself as its predecessor")]
    InvalidCommitChain,
    #[error("sampling fact is not bound to one stable source span")]
    InvalidFactSourceBinding,
    #[error("sampling commit contains {actual} facts; maximum is {max}")]
    TooManyFacts { max: usize, actual: usize },
    #[error("sampling archive record is {actual_bytes} bytes; maximum is {max_bytes}")]
    RecordTooLarge {
        max_bytes: usize,
        actual_bytes: usize,
    },
    #[error("sampling commit {commit_id:?} was replayed with a conflicting digest")]
    ConflictingCommit { commit_id: SamplingCommitId },
    #[error("sampling archive ledger is corrupted")]
    LedgerCorrupted,
    #[error("invalid executed Spine fact: {0}")]
    InvalidFact(#[source] crate::executed_fact::ExecutedFactError),
    #[error("invalid context plan: {0}")]
    InvalidPlan(#[source] ContextPlanError),
    #[error("sampling source digest does not match its context plan")]
    SourceDigestMismatch,
    #[error("failed to encode sampling archive record: {0}")]
    Serialize(#[source] serde_json::Error),
    #[error("failed to decode sampling archive record: {0}")]
    Deserialize(#[source] serde_json::Error),
}
