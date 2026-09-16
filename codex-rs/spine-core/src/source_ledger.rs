use crate::BoundaryId;
use crate::ContextEpoch;
use crate::ContextItem;
use crate::Message;
use crate::NativeItemRef;
use crate::RawBoundary;
use crate::RecordDigest;
use crate::SourceCellId;
use crate::SpineChar;
use crate::ThreadNamespace;
use crate::context_plan::ContextPlanSource;
use serde::Serialize;
use sha2::Digest as _;
use sha2::Sha256;
use thiserror::Error;

pub const MAX_SOURCE_SNAPSHOT_BYTES: usize = crate::MAX_RAW_EVENT_BYTES;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceLedger {
    thread: ThreadNamespace,
    epoch: ContextEpoch,
    cells: Vec<SourceCell>,
    last_raw_boundary: Option<RawBoundary>,
    next_ordinal: u64,
    digest: RecordDigest,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceSnapshot {
    thread: ThreadNamespace,
    epoch: ContextEpoch,
    cells: Vec<SourceCell>,
    digest: RecordDigest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SourceCell {
    pub id: SourceCellId,
    pub boundary: BoundaryId,
    pub payload: SourceCellPayload,
    pub item: ContextItem,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SourceCellPayload {
    Message(Message),
    Opaque,
    Synthetic(ContextItem),
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SourceLedgerError {
    #[error("source snapshot is {actual_bytes} bytes; maximum is {max_bytes} bytes")]
    SnapshotTooLarge {
        max_bytes: usize,
        actual_bytes: usize,
    },
    #[error("source boundary {} precedes {}", next.0, previous.0)]
    NonMonotonicBoundary {
        previous: RawBoundary,
        next: RawBoundary,
    },
    #[error(
        "source epoch {} cannot advance to {}",
        current.value(),
        next.value()
    )]
    EpochNotNext {
        current: ContextEpoch,
        next: ContextEpoch,
    },
    #[error("source epoch is exhausted")]
    EpochExhausted,
    #[error("source continuation prefix {committed} exceeds {actual} cells")]
    InvalidContinuationPrefix { committed: usize, actual: usize },
    #[error("failed to serialize source ledger: {0}")]
    Serialize(String),
}

impl SourceLedger {
    pub fn new(thread: ThreadNamespace, epoch: ContextEpoch) -> Result<Self, SourceLedgerError> {
        let digest = digest_cells(&[])?;
        Ok(Self {
            thread,
            epoch,
            cells: Vec::new(),
            last_raw_boundary: None,
            next_ordinal: 0,
            digest,
        })
    }

    pub fn thread(&self) -> &ThreadNamespace {
        &self.thread
    }

    pub const fn epoch(&self) -> ContextEpoch {
        self.epoch
    }

    pub fn digest(&self) -> &RecordDigest {
        &self.digest
    }

    pub fn snapshot(&self) -> SourceSnapshot {
        SourceSnapshot {
            thread: self.thread.clone(),
            epoch: self.epoch,
            cells: self.cells.clone(),
            digest: self.digest.clone(),
        }
    }

    pub fn append<I>(&mut self, characters: I) -> Result<Vec<SourceCellId>, SourceLedgerError>
    where
        I: IntoIterator<Item = SpineChar>,
    {
        let mut candidate = self.clone();
        let mut inserted = Vec::new();
        for character in characters {
            inserted.push(candidate.append_one(character)?);
        }
        candidate.digest = digest_cells(&candidate.cells)?;
        *self = candidate;
        Ok(inserted)
    }

    pub fn advance_epoch(
        &mut self,
        next: ContextEpoch,
    ) -> Result<SourceSnapshot, SourceLedgerError> {
        let expected = self
            .epoch
            .checked_next()
            .ok_or(SourceLedgerError::EpochExhausted)?;
        if next != expected {
            return Err(SourceLedgerError::EpochNotNext {
                current: self.epoch,
                next,
            });
        }
        let archived = self.snapshot();
        *self = Self::new(self.thread.clone(), next)?;
        Ok(archived)
    }

    pub(crate) fn continue_in_namespace(
        &mut self,
        thread: ThreadNamespace,
        committed_prefix: usize,
    ) -> Result<(), SourceLedgerError> {
        if committed_prefix > self.cells.len() {
            return Err(SourceLedgerError::InvalidContinuationPrefix {
                committed: committed_prefix,
                actual: self.cells.len(),
            });
        }
        if thread == self.thread {
            return Ok(());
        }
        let mut candidate = self.clone();
        candidate.thread = thread.clone();
        candidate.next_ordinal = 0;
        for cell in &mut candidate.cells[committed_prefix..] {
            cell.id = SourceCellId::new(thread.clone(), candidate.epoch, candidate.next_ordinal);
            cell.boundary =
                BoundaryId::new(thread.clone(), candidate.epoch, cell.boundary.ordinal());
            candidate.next_ordinal = candidate.next_ordinal.saturating_add(1);
        }
        candidate.digest = digest_cells(&candidate.cells)?;
        *self = candidate;
        Ok(())
    }

    pub(crate) fn current_boundary(&self) -> BoundaryId {
        BoundaryId::new(
            self.thread.clone(),
            self.epoch,
            self.last_raw_boundary.map_or(0, |boundary| boundary.0),
        )
    }

    fn append_one(&mut self, character: SpineChar) -> Result<SourceCellId, SourceLedgerError> {
        let raw_boundary = character.boundary();
        if let Some(previous) = self.last_raw_boundary
            && raw_boundary <= previous
        {
            return Err(SourceLedgerError::NonMonotonicBoundary {
                previous,
                next: raw_boundary,
            });
        }
        let id = SourceCellId::new(self.thread.clone(), self.epoch, self.next_ordinal);
        let boundary = BoundaryId::new(self.thread.clone(), self.epoch, raw_boundary.0);
        let (payload, item) = source_parts(character);
        self.cells.push(SourceCell {
            id: id.clone(),
            boundary,
            payload,
            item,
        });
        self.last_raw_boundary = Some(raw_boundary);
        self.next_ordinal = self.next_ordinal.saturating_add(1);
        Ok(id)
    }
}

impl SourceSnapshot {
    pub fn cells(&self) -> &[SourceCell] {
        &self.cells
    }

    pub fn boundary(&self, source_id: &SourceCellId) -> Option<&BoundaryId> {
        self.cells
            .iter()
            .find(|cell| &cell.id == source_id)
            .map(|cell| &cell.boundary)
    }

    pub fn payload(&self, source_id: &SourceCellId) -> Option<&SourceCellPayload> {
        self.cells
            .iter()
            .find(|cell| &cell.id == source_id)
            .map(|cell| &cell.payload)
    }

    pub fn last_boundary(&self) -> Option<&BoundaryId> {
        self.cells.last().map(|cell| &cell.boundary)
    }

    pub fn source_at_raw_boundary(&self, boundary: RawBoundary) -> Option<&SourceCell> {
        self.cells
            .iter()
            .find(|cell| cell.boundary.ordinal() == boundary.0)
    }
}

impl SourceCell {
    pub fn character(&self) -> SpineChar {
        let boundary = RawBoundary(self.boundary.ordinal());
        match &self.payload {
            SourceCellPayload::Message(message) => SpineChar::Message(message.clone()),
            SourceCellPayload::Opaque => SpineChar::Opaque { boundary },
            SourceCellPayload::Synthetic(item) => SpineChar::Synthetic {
                boundary,
                item: item.clone(),
            },
        }
    }
}

impl ContextPlanSource for SourceSnapshot {
    fn thread(&self) -> &ThreadNamespace {
        &self.thread
    }

    fn epoch(&self) -> ContextEpoch {
        self.epoch
    }

    fn digest(&self) -> &RecordDigest {
        &self.digest
    }

    fn resolve(&self, source_id: &SourceCellId) -> Option<&ContextItem> {
        self.cells
            .iter()
            .find(|cell| &cell.id == source_id)
            .map(|cell| &cell.item)
    }
}

fn source_parts(character: SpineChar) -> (SourceCellPayload, ContextItem) {
    match character {
        SpineChar::Message(message) => (
            SourceCellPayload::Message(message.clone()),
            ContextItem::Message {
                message,
                user_anchor: None,
            },
        ),
        SpineChar::Opaque { boundary } => (SourceCellPayload::Opaque, native_item(boundary)),
        SpineChar::Synthetic { item, .. } => (SourceCellPayload::Synthetic(item.clone()), item),
    }
}

fn native_item(boundary: RawBoundary) -> ContextItem {
    ContextItem::Native {
        source: NativeItemRef::Rollout { ordinal: boundary },
    }
}

fn digest_cells(cells: &[SourceCell]) -> Result<RecordDigest, SourceLedgerError> {
    let encoded = serde_json::to_vec(cells)
        .map_err(|error| SourceLedgerError::Serialize(error.to_string()))?;
    if encoded.len() > MAX_SOURCE_SNAPSHOT_BYTES {
        return Err(SourceLedgerError::SnapshotTooLarge {
            max_bytes: MAX_SOURCE_SNAPSHOT_BYTES,
            actual_bytes: encoded.len(),
        });
    }
    RecordDigest::parse(format!("{:x}", Sha256::digest(encoded)))
        .map_err(|error| SourceLedgerError::Serialize(error.to_string()))
}
