use crate::CharParseError;
use crate::ContextPlanError;
use crate::ExecutedFactError;
use crate::Feature;
use crate::RawBoundary;
use crate::SourceCellId;
use crate::SourceLedgerError;
use crate::archive::ArchiveError;
use crate::sampling::SamplingError;
use std::fmt;

#[derive(Debug)]
pub enum PlannerError {
    Initialize(crate::InitError),
    Source(SourceLedgerError),
    Sampling(SamplingError),
    Parse(CharParseError),
    CompileSpine(crate::SpineError),
    InvalidTransition(PlannerTransitionError),
    InvalidFact(ExecutedFactError),
    ContextPlan(ContextPlanError),
    Archive(ArchiveError),
    SamplingAlreadyActive,
    JitDisabled,
    FeatureDisabled(Feature),
    NoActiveSampling,
    AttemptMismatch,
    IdentityScopeMismatch,
    InvalidBoundaryOrder,
    PostBoundaryIsNotSourceTail,
    CommittedSourcePrefixMissing,
    FactHasNoSourceGroup(crate::ExecutionId),
    MissingTrimSource(SourceCellId),
    MissingTrimBoundary(RawBoundary),
    TrimRuntimeUnavailable,
    InvalidTrim(String),
    MissingSourceBoundary(RawBoundary),
    ArchivedSourceInLivePlan,
    UncommittedSourceAtCompact,
    InvalidCompactBarrier(String),
    SamplingNotStarted,
    SamplingAlreadyStarted,
    SamplingCommitPendingInstall,
    PreparedSamplingMismatch,
    PreparedSamplingStale,
    DuplicateExecutionKey(String),
    UnknownExecutionKey(String),
    ExecutionAlreadyStaged(String),
    SuccessfulExecutionMissingFact(String),
    PendingExecutions(usize),
}

impl fmt::Display for PlannerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Initialize(error) => write!(formatter, "failed to initialize planner: {error}"),
            Self::Source(error) => write!(formatter, "source ledger failed: {error}"),
            Self::Sampling(error) => write!(formatter, "sampling transaction failed: {error}"),
            Self::Parse(error) => write!(formatter, "source parsing failed: {error}"),
            Self::CompileSpine(error) => {
                write!(formatter, "sampling compilation failed: {error}")
            }
            Self::InvalidTransition(error) => {
                write!(formatter, "invalid sampling transition: {error}")
            }
            Self::InvalidFact(error) => write!(formatter, "invalid executed fact: {error}"),
            Self::ContextPlan(error) => write!(formatter, "context planning failed: {error}"),
            Self::Archive(error) => write!(formatter, "archive preparation failed: {error}"),
            Self::SamplingAlreadyActive => formatter.write_str("a sampling attempt is active"),
            Self::JitDisabled => formatter.write_str("sampling requires the JIT feature"),
            Self::FeatureDisabled(feature) => {
                write!(
                    formatter,
                    "sampling fact requires disabled feature {feature:?}"
                )
            }
            Self::NoActiveSampling => formatter.write_str("no sampling attempt is active"),
            Self::AttemptMismatch => {
                formatter.write_str("sampling handle is not the active attempt")
            }
            Self::IdentityScopeMismatch => {
                formatter.write_str("sampling identities belong to another thread or epoch")
            }
            Self::InvalidBoundaryOrder => {
                formatter.write_str("sampling boundaries are not monotonic")
            }
            Self::PostBoundaryIsNotSourceTail => {
                formatter.write_str("sampling post-boundary is not the source tail")
            }
            Self::CommittedSourcePrefixMissing => {
                formatter.write_str("source snapshot lost the committed prefix")
            }
            Self::FactHasNoSourceGroup(execution) => write!(
                formatter,
                "executed fact {} has no matching source group",
                execution.as_str()
            ),
            Self::MissingTrimSource(source_id) => {
                write!(formatter, "trim source {source_id:?} is missing")
            }
            Self::MissingTrimBoundary(boundary) => {
                write!(
                    formatter,
                    "trim target at source boundary {} is missing",
                    boundary.0
                )
            }
            Self::TrimRuntimeUnavailable => formatter.write_str("trim runtime is unavailable"),
            Self::InvalidTrim(error) => write!(formatter, "invalid trim fact: {error}"),
            Self::MissingSourceBoundary(boundary) => {
                write!(formatter, "source boundary {} is missing", boundary.0)
            }
            Self::ArchivedSourceInLivePlan => {
                formatter.write_str("archived compact source cannot enter a live context plan")
            }
            Self::UncommittedSourceAtCompact => {
                formatter.write_str("compact cannot cross uncommitted source cells")
            }
            Self::InvalidCompactBarrier(error) => {
                write!(formatter, "invalid compact barrier: {error}")
            }
            Self::SamplingNotStarted => {
                formatter.write_str("sampling attempt has no durable sampling-started record")
            }
            Self::SamplingAlreadyStarted => formatter
                .write_str("sampling attempt already has a durable sampling-started record"),
            Self::SamplingCommitPendingInstall => {
                formatter.write_str("a prepared sampling commit is awaiting installation")
            }
            Self::PreparedSamplingMismatch => {
                formatter.write_str("prepared sampling commit is not the pending commit")
            }
            Self::PreparedSamplingStale => {
                formatter.write_str("prepared sampling commit is based on stale runtime state")
            }
            Self::DuplicateExecutionKey(key) => {
                write!(formatter, "execution `{key}` is already registered")
            }
            Self::UnknownExecutionKey(key) => {
                write!(formatter, "execution `{key}` is not registered")
            }
            Self::ExecutionAlreadyStaged(key) => {
                write!(formatter, "execution `{key}` already staged a fact")
            }
            Self::SuccessfulExecutionMissingFact(key) => {
                write!(formatter, "successful execution `{key}` staged no fact")
            }
            Self::PendingExecutions(count) => {
                write!(formatter, "sampling sealed with {count} pending executions")
            }
        }
    }
}

impl std::error::Error for PlannerError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlannerTransitionError {
    MultipleStructuralFacts,
    TaskCursorRequired(&'static str),
    NonTrimFactInTrimSet,
    InactiveTrimTarget(RawBoundary),
    TrimTargetMismatch,
}

impl fmt::Display for PlannerTransitionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MultipleStructuralFacts => {
                formatter.write_str("sampling contains multiple structural facts")
            }
            Self::TaskCursorRequired(operation) => {
                write!(formatter, "{operation} requires an active task cursor")
            }
            Self::NonTrimFactInTrimSet => {
                formatter.write_str("trim set contains a structural fact")
            }
            Self::InactiveTrimTarget(boundary) => {
                write!(formatter, "trim target {} is not active", boundary.0)
            }
            Self::TrimTargetMismatch => {
                formatter.write_str("trim target identity does not match its source")
            }
        }
    }
}
