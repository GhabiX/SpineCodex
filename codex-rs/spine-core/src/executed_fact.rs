use crate::MAX_MEMORY_BYTES;
use crate::MAX_SUMMARY_BYTES;
use crate::identity::AdmissionOrdinal;
use crate::identity::ExecutionId;
use crate::identity::SourceCellId;
use crate::identity::TrimTicket;
use crate::model::SpawnReceipt;
use crate::model::SpawnResult;
use crate::model::SpawnTask;
use crate::model::SpawnValidationError;
use crate::model::TrimEdit;
use serde::Deserialize;
use serde::Serialize;
use std::fmt;

pub const MAX_EXECUTION_ORIGIN_BYTES: usize = 1024;
pub const MAX_EXECUTED_FACT_PAYLOAD_BYTES: usize = 160 * 1024;
pub const MAX_SOURCE_DIGEST_BYTES: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutedSpineFact {
    pub execution_id: ExecutionId,
    pub ordinal: AdmissionOrdinal,
    pub origin: ExecutionOrigin,
    pub operation: SpineOperationFact,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExecutionOrigin {
    Direct { call_id: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SpineOperationFact {
    Open {
        summary: String,
    },
    Close {
        memory: String,
    },
    Next {
        closed_memory: String,
        next_summary: String,
    },
    Spawn {
        tasks: Vec<SpawnTask>,
        terminal_results: Vec<SpawnResult>,
    },
    Trim {
        ticket: TrimTicket,
        target: StableToolOutputId,
        validated_edit: TrimEdit,
        source_digest: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StableToolOutputId {
    pub request: SourceCellId,
    pub response: SourceCellId,
    pub call_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecutedFactError {
    EmptyField(&'static str),
    FieldTooLarge {
        field: &'static str,
        max_bytes: usize,
        actual_bytes: usize,
    },
    PayloadTooLarge {
        max_bytes: usize,
        actual_bytes: usize,
    },
    InvalidSpawn(SpawnValidationError),
    InvalidTrimTicket(&'static str),
    InvalidTrimEdit(&'static str),
    Serialize(String),
}

impl ExecutedSpineFact {
    pub fn validate(&self) -> Result<(), ExecutedFactError> {
        validate_origin(&self.origin)?;
        match &self.operation {
            SpineOperationFact::Open { summary } => {
                validate_field("summary", summary, MAX_SUMMARY_BYTES)?;
            }
            SpineOperationFact::Close { memory } => {
                validate_field("memory", memory, MAX_MEMORY_BYTES)?;
            }
            SpineOperationFact::Next {
                closed_memory,
                next_summary,
            } => {
                validate_field("closed_memory", closed_memory, MAX_MEMORY_BYTES)?;
                validate_field("next_summary", next_summary, MAX_SUMMARY_BYTES)?;
            }
            SpineOperationFact::Spawn {
                tasks,
                terminal_results,
            } => {
                SpawnReceipt {
                    schema: crate::SPINE_SPAWN_RESULT_SCHEMA.to_string(),
                    results: terminal_results.clone(),
                }
                .validate_for(tasks)
                .map_err(ExecutedFactError::InvalidSpawn)?;
            }
            SpineOperationFact::Trim {
                ticket,
                target,
                validated_edit,
                source_digest,
            } => {
                validate_field("trim.call_id", &target.call_id, MAX_EXECUTION_ORIGIN_BYTES)?;
                validate_field("trim.source_digest", source_digest, MAX_SOURCE_DIGEST_BYTES)?;
                validate_trim_ticket(&self.execution_id, ticket, target)?;
                match validated_edit {
                    TrimEdit::Tagged { .. } => {
                        return Err(ExecutedFactError::InvalidTrimEdit(
                            "candidate markers are not validated replacement edits",
                        ));
                    }
                    TrimEdit::Snipped | TrimEdit::Sliced(_) => {}
                }
            }
        }

        let actual_bytes = serde_json::to_vec(self)
            .map_err(|error| ExecutedFactError::Serialize(error.to_string()))?
            .len();
        if actual_bytes > MAX_EXECUTED_FACT_PAYLOAD_BYTES {
            return Err(ExecutedFactError::PayloadTooLarge {
                max_bytes: MAX_EXECUTED_FACT_PAYLOAD_BYTES,
                actual_bytes,
            });
        }
        Ok(())
    }
}

fn validate_origin(origin: &ExecutionOrigin) -> Result<(), ExecutedFactError> {
    match origin {
        ExecutionOrigin::Direct { call_id } => {
            validate_field("origin.call_id", call_id, MAX_EXECUTION_ORIGIN_BYTES)
        }
    }
}

fn validate_trim_ticket(
    execution_id: &ExecutionId,
    ticket: &TrimTicket,
    target: &StableToolOutputId,
) -> Result<(), ExecutedFactError> {
    if target.request.epoch() != target.response.epoch() {
        return Err(ExecutedFactError::InvalidTrimTicket(
            "request and response must belong to the same epoch",
        ));
    }
    if target.request.epoch() != ticket.epoch() {
        return Err(ExecutedFactError::InvalidTrimTicket(
            "ticket and target must belong to the same epoch",
        ));
    }
    if target.request.thread() != target.response.thread()
        || target.request.thread() != ticket.thread()
        || target.request.thread() != execution_id.thread()
    {
        return Err(ExecutedFactError::InvalidTrimTicket(
            "execution, ticket, and target must belong to the same thread",
        ));
    }
    if target.request.ordinal() >= target.response.ordinal() {
        return Err(ExecutedFactError::InvalidTrimTicket(
            "tool request must precede its response",
        ));
    }
    Ok(())
}

fn validate_field(
    field: &'static str,
    value: &str,
    max_bytes: usize,
) -> Result<(), ExecutedFactError> {
    if value.trim().is_empty() {
        return Err(ExecutedFactError::EmptyField(field));
    }
    if value.len() > max_bytes {
        return Err(ExecutedFactError::FieldTooLarge {
            field,
            max_bytes,
            actual_bytes: value.len(),
        });
    }
    Ok(())
}

impl fmt::Display for ExecutedFactError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyField(field) => write!(f, "{field} must not be empty"),
            Self::FieldTooLarge {
                field,
                max_bytes,
                actual_bytes,
            } => write!(
                f,
                "{field} is {actual_bytes} bytes; maximum is {max_bytes} bytes"
            ),
            Self::PayloadTooLarge {
                max_bytes,
                actual_bytes,
            } => write!(
                f,
                "executed Spine fact is {actual_bytes} bytes; maximum is {max_bytes} bytes"
            ),
            Self::InvalidSpawn(error) => write!(f, "{error}"),
            Self::InvalidTrimTicket(reason) => write!(f, "invalid trim ticket: {reason}"),
            Self::InvalidTrimEdit(reason) => write!(f, "invalid trim edit: {reason}"),
            Self::Serialize(error) => write!(f, "failed to serialize executed Spine fact: {error}"),
        }
    }
}

impl std::error::Error for ExecutedFactError {}
