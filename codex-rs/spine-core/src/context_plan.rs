use crate::ContextEpoch;
use crate::ContextItem;
use crate::ContextLabel;
use crate::MAX_SYNTHETIC_CONTEXT_BYTES;
use crate::MAX_VISIBLE_CONTEXT_ITEMS;
use crate::MemorySlot;
use crate::ProjectionCellId;
use crate::RecordDigest;
use crate::SourceCellId;
use crate::ThreadNamespace;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest as _;
use sha2::Sha256;
use std::collections::BTreeSet;
use std::fmt;

pub const CONTEXT_PLAN_SCHEMA_V1: &str = "spine.context.plan.v1";
pub const MAX_CONTEXT_PLAN_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_CONTEXT_PLAN_MEMORY_SLOTS: usize = MAX_VISIBLE_CONTEXT_ITEMS;
pub const MAX_CONTEXT_LABELS_PER_SOURCE: usize = 2;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextPlanRecipe {
    pub schema: String,
    pub thread: ThreadNamespace,
    pub epoch: ContextEpoch,
    pub source_snapshot_digest: RecordDigest,
    pub cells: Vec<ContextPlanCell>,
    pub memory_slots: Vec<MemorySlot>,
    pub plan_digest: RecordDigest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ContextPlanCell {
    Source {
        source_id: SourceCellId,
        labels: Vec<ContextLabel>,
    },
    Projection {
        projection_id: ProjectionCellId,
        item: ContextItem,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedContextPlan {
    pub cells: Vec<ResolvedContextCell>,
    pub memory_slots: Vec<MemorySlot>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedContextCell {
    pub provenance: ContextCellProvenance,
    pub item: ContextItem,
    pub labels: Vec<ContextLabel>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContextCellProvenance {
    Source(SourceCellId),
    Projection(ProjectionCellId),
}

/// Resolves stable source identities from one immutable source snapshot.
///
/// Implementations must expose one thread and epoch and must return the same
/// semantic item for an identity throughout a plan validation and resolution.
pub trait ContextPlanSource {
    fn thread(&self) -> &ThreadNamespace;
    fn epoch(&self) -> ContextEpoch;
    fn digest(&self) -> &RecordDigest;
    fn resolve(&self, source_id: &SourceCellId) -> Option<&ContextItem>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContextPlanError {
    UnsupportedSchema {
        actual: String,
    },
    IdentityScopeMismatch,
    TooManyCells {
        max: usize,
        actual: usize,
    },
    TooManyMemorySlots {
        max: usize,
        actual: usize,
    },
    DuplicateSourceCell(SourceCellId),
    DuplicateProjectionCell(ProjectionCellId),
    InvalidSourceLabels {
        source_id: SourceCellId,
    },
    SyntheticContextTooLarge {
        max_bytes: usize,
        actual_bytes: usize,
    },
    RecipeTooLarge {
        max_bytes: usize,
        actual_bytes: usize,
    },
    DigestMismatch {
        expected: RecordDigest,
        actual: RecordDigest,
    },
    SourceSnapshotScopeMismatch,
    SourceSnapshotDigestMismatch,
    MissingSourceCell(SourceCellId),
    Serialize(String),
}

impl ContextPlanRecipe {
    pub fn finalize_digest(mut self) -> Result<Self, ContextPlanError> {
        self.validate_structure()?;
        self.plan_digest = self.computed_digest()?;
        self.validate()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), ContextPlanError> {
        self.validate_structure()?;
        let expected = self.computed_digest()?;
        if self.plan_digest != expected {
            return Err(ContextPlanError::DigestMismatch {
                expected,
                actual: self.plan_digest.clone(),
            });
        }
        Ok(())
    }

    pub fn resolve<S>(&self, source: &S) -> Result<ResolvedContextPlan, ContextPlanError>
    where
        S: ContextPlanSource,
    {
        self.validate()?;
        if source.thread() != &self.thread || source.epoch() != self.epoch {
            return Err(ContextPlanError::SourceSnapshotScopeMismatch);
        }
        if source.digest() != &self.source_snapshot_digest {
            return Err(ContextPlanError::SourceSnapshotDigestMismatch);
        }

        let cells = self
            .cells
            .iter()
            .map(|cell| match cell {
                ContextPlanCell::Source { source_id, labels } => source
                    .resolve(source_id)
                    .cloned()
                    .map(|item| ResolvedContextCell {
                        provenance: ContextCellProvenance::Source(source_id.clone()),
                        item,
                        labels: labels.clone(),
                    })
                    .ok_or_else(|| ContextPlanError::MissingSourceCell(source_id.clone())),
                ContextPlanCell::Projection {
                    projection_id,
                    item,
                } => Ok(ResolvedContextCell {
                    provenance: ContextCellProvenance::Projection(projection_id.clone()),
                    item: item.clone(),
                    labels: Vec::new(),
                }),
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ResolvedContextPlan {
            cells,
            memory_slots: self.memory_slots.clone(),
        })
    }

    fn validate_structure(&self) -> Result<(), ContextPlanError> {
        if self.schema != CONTEXT_PLAN_SCHEMA_V1 {
            return Err(ContextPlanError::UnsupportedSchema {
                actual: self.schema.clone(),
            });
        }
        if self.cells.len() > MAX_VISIBLE_CONTEXT_ITEMS {
            return Err(ContextPlanError::TooManyCells {
                max: MAX_VISIBLE_CONTEXT_ITEMS,
                actual: self.cells.len(),
            });
        }
        if self.memory_slots.len() > MAX_CONTEXT_PLAN_MEMORY_SLOTS {
            return Err(ContextPlanError::TooManyMemorySlots {
                max: MAX_CONTEXT_PLAN_MEMORY_SLOTS,
                actual: self.memory_slots.len(),
            });
        }

        let mut source_ids = BTreeSet::new();
        let mut projection_ids = BTreeSet::new();
        for cell in &self.cells {
            match cell {
                ContextPlanCell::Source { source_id, labels } => {
                    if source_id.epoch() != self.epoch {
                        return Err(ContextPlanError::IdentityScopeMismatch);
                    }
                    if !source_ids.insert(source_id.clone()) {
                        return Err(ContextPlanError::DuplicateSourceCell(source_id.clone()));
                    }
                    if labels.len() > MAX_CONTEXT_LABELS_PER_SOURCE
                        || labels
                            .iter()
                            .enumerate()
                            .any(|(index, label)| labels[index + 1..].contains(label))
                    {
                        return Err(ContextPlanError::InvalidSourceLabels {
                            source_id: source_id.clone(),
                        });
                    }
                }
                ContextPlanCell::Projection {
                    projection_id,
                    item: _,
                } => {
                    self.validate_scope(projection_id.thread(), projection_id.epoch())?;
                    if !projection_ids.insert(projection_id.clone()) {
                        return Err(ContextPlanError::DuplicateProjectionCell(
                            projection_id.clone(),
                        ));
                    }
                }
            }
        }

        let actual_synthetic_bytes = self
            .cells
            .iter()
            .filter_map(|cell| match cell {
                ContextPlanCell::Source { .. } => None,
                ContextPlanCell::Projection { item, .. } => Some(item.retained_synthetic_bytes()),
            })
            .chain(
                self.memory_slots
                    .iter()
                    .cloned()
                    .map(ContextItem::MemorySlot)
                    .map(|item| item.retained_synthetic_bytes()),
            )
            .fold(0usize, usize::saturating_add);
        if actual_synthetic_bytes > MAX_SYNTHETIC_CONTEXT_BYTES {
            return Err(ContextPlanError::SyntheticContextTooLarge {
                max_bytes: MAX_SYNTHETIC_CONTEXT_BYTES,
                actual_bytes: actual_synthetic_bytes,
            });
        }

        let encoded = serde_json::to_vec(self)
            .map_err(|error| ContextPlanError::Serialize(error.to_string()))?;
        if encoded.len() > MAX_CONTEXT_PLAN_BYTES {
            return Err(ContextPlanError::RecipeTooLarge {
                max_bytes: MAX_CONTEXT_PLAN_BYTES,
                actual_bytes: encoded.len(),
            });
        }
        Ok(())
    }

    fn validate_scope(
        &self,
        thread: &ThreadNamespace,
        epoch: ContextEpoch,
    ) -> Result<(), ContextPlanError> {
        if thread != &self.thread || epoch != self.epoch {
            return Err(ContextPlanError::IdentityScopeMismatch);
        }
        Ok(())
    }

    fn computed_digest(&self) -> Result<RecordDigest, ContextPlanError> {
        let mut canonical = self.clone();
        canonical.plan_digest = RecordDigest::parse("0".repeat(64))
            .map_err(|error| ContextPlanError::Serialize(error.to_string()))?;
        let encoded = serde_json::to_vec(&canonical)
            .map_err(|error| ContextPlanError::Serialize(error.to_string()))?;
        RecordDigest::parse(format!("{:x}", Sha256::digest(encoded)))
            .map_err(|error| ContextPlanError::Serialize(error.to_string()))
    }
}

impl fmt::Display for ContextPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedSchema { actual } => {
                write!(formatter, "unsupported context plan schema {actual}")
            }
            Self::IdentityScopeMismatch => {
                formatter.write_str("context plan identity belongs to another thread or epoch")
            }
            Self::TooManyCells { max, actual } => {
                write!(
                    formatter,
                    "context plan has {actual} cells; maximum is {max}"
                )
            }
            Self::TooManyMemorySlots { max, actual } => write!(
                formatter,
                "context plan has {actual} memory slots; maximum is {max}"
            ),
            Self::DuplicateSourceCell(source_id) => {
                write!(formatter, "duplicate source cell {source_id:?}")
            }
            Self::DuplicateProjectionCell(projection_id) => {
                write!(formatter, "duplicate projection cell {projection_id:?}")
            }
            Self::InvalidSourceLabels { source_id } => {
                write!(formatter, "invalid labels for source cell {source_id:?}")
            }
            Self::SyntheticContextTooLarge {
                max_bytes,
                actual_bytes,
            } => write!(
                formatter,
                "context plan retains {actual_bytes} synthetic bytes; maximum is {max_bytes}"
            ),
            Self::RecipeTooLarge {
                max_bytes,
                actual_bytes,
            } => write!(
                formatter,
                "context plan is {actual_bytes} bytes; maximum is {max_bytes}"
            ),
            Self::DigestMismatch { .. } => {
                formatter.write_str("context plan digest does not match its payload")
            }
            Self::SourceSnapshotScopeMismatch => {
                formatter.write_str("source snapshot belongs to another thread or epoch")
            }
            Self::SourceSnapshotDigestMismatch => {
                formatter.write_str("source snapshot digest does not match the context plan")
            }
            Self::MissingSourceCell(source_id) => {
                write!(formatter, "source snapshot is missing cell {source_id:?}")
            }
            Self::Serialize(error) => {
                write!(formatter, "failed to serialize context plan: {error}")
            }
        }
    }
}

impl std::error::Error for ContextPlanError {}
