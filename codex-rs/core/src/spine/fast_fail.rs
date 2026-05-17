use super::mem_install::MemoryBodyError;
use thiserror::Error;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub(crate) enum RuntimeFastFailError {
    #[error(
        "I6 every committed Mem has span metadata and verified body: MemInstall {compact_id} has no matching CompactStarted span"
    )]
    MemInstallMissingStarted { compact_id: String },
    #[error(
        "I6 every committed Mem has span metadata and verified body: MemInstall {compact_id} has duplicate compact_id"
    )]
    MemInstallDuplicateCompactId { compact_id: String },
    #[error(
        "I11 suffix/root install never exposes host checkpoint before semantic commit: MemInstall {compact_id} follows {terminal}"
    )]
    MemInstallCheckpointBeforeCommit {
        compact_id: String,
        terminal: &'static str,
    },
    #[error(
        "I11 suffix/root install never exposes host checkpoint before semantic commit: MemInstall {compact_id} is followed by invalid terminal {terminal}"
    )]
    MemInstallInvalidTerminalAfterCommit {
        compact_id: String,
        terminal: &'static str,
    },
    #[error(
        "I6 every committed Mem has span metadata and verified body: MemInstall {compact_id} does not match CompactStarted span metadata"
    )]
    MemInstallSpanMismatch { compact_id: String },
    #[error(
        "I6 every committed Mem has span metadata and verified body: MemInstall {compact_id} is missing projection_ref"
    )]
    MemInstallMissingProjectionRef { compact_id: String },
    #[error(
        "I6 every committed Mem has span metadata and verified body: MemInstall {compact_id} is missing source_rollout_ref"
    )]
    MemInstallMissingSourceRolloutRef { compact_id: String },
    #[error(
        "I6 every committed Mem has span metadata and verified body: MemInstall {compact_id} source_rollout_ref does not match CompactStarted rollout"
    )]
    MemInstallSourceRolloutMismatch { compact_id: String },
    #[error(
        "I6 every committed Mem has span metadata and verified body: MemInstall {compact_id} has malformed memory body ref {memory_section_id:?} for storage {storage_ref}"
    )]
    MemInstallMalformedBodyRef {
        compact_id: String,
        memory_section_id: String,
        storage_ref: String,
    },
    #[error(
        "I6 every committed Mem has span metadata and verified body: MemInstall {compact_id} body section {section_id} is missing"
    )]
    MemInstallMissingBody {
        compact_id: String,
        section_id: String,
    },
    #[error(
        "I6 every committed Mem has span metadata and verified body: MemInstall {compact_id} body hash mismatch for {section_id}: expected {expected}, actual {actual}"
    )]
    MemInstallBodyHashMismatch {
        compact_id: String,
        section_id: String,
        expected: String,
        actual: String,
    },
    #[error(
        "I6 every committed Mem has span metadata and verified body: MemInstall {compact_id} body storage mismatch for {section_id}: expected {expected}, actual {actual}"
    )]
    MemInstallBodyStorageMismatch {
        compact_id: String,
        section_id: String,
        expected: String,
        actual: String,
    },
    #[error(
        "I6 every committed Mem has span metadata and verified body: MemInstall {compact_id} has unsupported schema_version {schema_version}"
    )]
    MemInstallUnsupportedSchema {
        compact_id: String,
        schema_version: u32,
    },
    #[error(
        "I7 pending install artifacts invisible to Project(Pi): MemInstall {compact_id} committed_at_seq {actual}, expected {expected}"
    )]
    MemInstallCommittedSeqMismatch {
        compact_id: String,
        expected: u64,
        actual: u64,
    },
}

pub(crate) fn validate_mem_install_pre_commit(
    compact_id: &str,
    has_started: bool,
    duplicate_commit: bool,
    terminal_before_commit: Option<&'static str>,
    projection_ref: &str,
    source_rollout_ref: &str,
    source_rollout_matches_started: bool,
) -> Result<(), RuntimeFastFailError> {
    if duplicate_commit {
        return Err(RuntimeFastFailError::MemInstallDuplicateCompactId {
            compact_id: compact_id.to_string(),
        });
    }
    if let Some(terminal) = terminal_before_commit {
        return Err(RuntimeFastFailError::MemInstallCheckpointBeforeCommit {
            compact_id: compact_id.to_string(),
            terminal,
        });
    }
    if !has_started {
        return Err(RuntimeFastFailError::MemInstallMissingStarted {
            compact_id: compact_id.to_string(),
        });
    }
    validate_mem_install_metadata(
        compact_id,
        projection_ref,
        source_rollout_ref,
        source_rollout_matches_started,
    )
}

pub(crate) fn validate_mem_install_metadata(
    compact_id: &str,
    projection_ref: &str,
    source_rollout_ref: &str,
    source_rollout_matches_started: bool,
) -> Result<(), RuntimeFastFailError> {
    if projection_ref.trim().is_empty() {
        return Err(RuntimeFastFailError::MemInstallMissingProjectionRef {
            compact_id: compact_id.to_string(),
        });
    }
    if source_rollout_ref.trim().is_empty() {
        return Err(RuntimeFastFailError::MemInstallMissingSourceRolloutRef {
            compact_id: compact_id.to_string(),
        });
    }
    if !source_rollout_matches_started {
        return Err(RuntimeFastFailError::MemInstallSourceRolloutMismatch {
            compact_id: compact_id.to_string(),
        });
    }
    Ok(())
}

pub(crate) fn mem_install_body_error(
    compact_id: &str,
    error: MemoryBodyError,
) -> RuntimeFastFailError {
    match error {
        MemoryBodyError::MalformedSectionId {
            memory_section_id,
            storage_ref,
        } => RuntimeFastFailError::MemInstallMalformedBodyRef {
            compact_id: compact_id.to_string(),
            memory_section_id,
            storage_ref,
        },
        MemoryBodyError::MissingSection { section_id } => {
            RuntimeFastFailError::MemInstallMissingBody {
                compact_id: compact_id.to_string(),
                section_id: section_id.to_string(),
            }
        }
        MemoryBodyError::StorageMismatch {
            section_id,
            expected,
            actual,
        } => RuntimeFastFailError::MemInstallBodyStorageMismatch {
            compact_id: compact_id.to_string(),
            section_id: section_id.to_string(),
            expected,
            actual,
        },
        MemoryBodyError::BodyHashMismatch {
            section_id,
            expected,
            actual,
        } => RuntimeFastFailError::MemInstallBodyHashMismatch {
            compact_id: compact_id.to_string(),
            section_id: section_id.to_string(),
            expected,
            actual,
        },
    }
}
