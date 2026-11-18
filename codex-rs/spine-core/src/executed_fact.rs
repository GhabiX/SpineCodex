use crate::MAX_MEMORY_BYTES;
use crate::MAX_SUMMARY_BYTES;
use crate::identity::AdmissionOrdinal;
use crate::identity::ExecutionId;
use crate::model::SpawnReceipt;
use crate::model::SpawnResult;
use crate::model::SpawnTask;
use crate::model::SpawnValidationError;
use serde::Deserialize;
use serde::Serialize;
use std::fmt;

pub const MAX_EXECUTION_ORIGIN_BYTES: usize = 1024;
pub const MAX_EXECUTED_FACT_PAYLOAD_BYTES: usize = 160 * 1024;

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
    Direct { execution_ref: String },
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
        ExecutionOrigin::Direct { execution_ref } => validate_field(
            "origin.execution_ref",
            execution_ref,
            MAX_EXECUTION_ORIGIN_BYTES,
        ),
    }
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
            Self::Serialize(error) => write!(f, "failed to serialize executed Spine fact: {error}"),
        }
    }
}

impl std::error::Error for ExecutedFactError {}
