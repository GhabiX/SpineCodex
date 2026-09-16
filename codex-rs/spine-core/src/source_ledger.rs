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
const SOURCE_DIGEST_V2_DOMAIN: &[u8] = b"spine.source.v2\0";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceDigestVersion {
    V1,
    V2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SourceDigestLock {
    Unlocked,
    V1,
    V2,
}

#[derive(Clone, Debug)]
pub struct SourceLedger {
    thread: ThreadNamespace,
    epoch: ContextEpoch,
    cells: Vec<SourceCell>,
    last_raw_boundary: Option<RawBoundary>,
    next_ordinal: u64,
    digest: RecordDigest,
    lock: SourceDigestLock,
    hasher: Sha256,
    encoded_size: usize,
}

impl PartialEq for SourceLedger {
    fn eq(&self, other: &Self) -> bool {
        self.thread == other.thread
            && self.epoch == other.epoch
            && self.cells == other.cells
            && self.digest == other.digest
            && self.lock == other.lock
    }
}

impl Eq for SourceLedger {}

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
    #[error("source digest does not match the ledger")]
    DigestMismatch,
}

impl SourceLedger {
    pub fn new(thread: ThreadNamespace, epoch: ContextEpoch) -> Result<Self, SourceLedgerError> {
        Self::new_with_version(thread, epoch, SourceDigestVersion::V1)
    }

    pub fn new_with_version(
        thread: ThreadNamespace,
        epoch: ContextEpoch,
        version: SourceDigestVersion,
    ) -> Result<Self, SourceLedgerError> {
        match version {
            SourceDigestVersion::V1 => Self::from_lock(thread, epoch, SourceDigestLock::V1),
            SourceDigestVersion::V2 => Self::from_lock(thread, epoch, SourceDigestLock::V2),
        }
    }

    pub(crate) fn new_unlocked(
        thread: ThreadNamespace,
        epoch: ContextEpoch,
    ) -> Result<Self, SourceLedgerError> {
        Self::from_lock(thread, epoch, SourceDigestLock::Unlocked)
    }

    fn from_lock(
        thread: ThreadNamespace,
        epoch: ContextEpoch,
        lock: SourceDigestLock,
    ) -> Result<Self, SourceLedgerError> {
        let (digest, hasher, encoded_size) = match lock {
            SourceDigestLock::V2 => digest_v2(&[])?,
            SourceDigestLock::V1 | SourceDigestLock::Unlocked => {
                (digest_cells(&[])?, v2_hasher(), 0)
            }
        };
        Ok(Self {
            thread,
            epoch,
            cells: Vec::new(),
            last_raw_boundary: None,
            next_ordinal: 0,
            digest,
            lock,
            hasher,
            encoded_size,
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
        let mut inserted = Vec::new();
        for character in characters {
            inserted.push(self.append_one(character)?);
        }
        if self.lock == SourceDigestLock::V1 {
            self.digest = digest_cells(&self.cells)?;
        }
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
        *self = Self::from_lock(self.thread.clone(), next, self.lock)?;
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
        self.thread = thread.clone();
        self.next_ordinal = 0;
        for cell in &mut self.cells[committed_prefix..] {
            cell.id = SourceCellId::new(thread.clone(), self.epoch, self.next_ordinal);
            cell.boundary = BoundaryId::new(thread.clone(), self.epoch, cell.boundary.ordinal());
            self.next_ordinal = self.next_ordinal.saturating_add(1);
        }
        self.recompute_locked_digest()
    }

    pub(crate) fn lock_or_check_digest(
        &mut self,
        expected: &RecordDigest,
    ) -> Result<(), SourceLedgerError> {
        match self.lock {
            SourceDigestLock::Unlocked => {
                let v1 = digest_cells(&self.cells)?;
                if &v1 == expected {
                    self.lock = SourceDigestLock::V1;
                    self.digest = v1;
                    return Ok(());
                }
                let (v2, hasher, encoded_size) = digest_v2(&self.cells)?;
                if &v2 == expected {
                    self.lock = SourceDigestLock::V2;
                    self.digest = v2;
                    self.hasher = hasher;
                    self.encoded_size = encoded_size;
                    return Ok(());
                }
                Err(SourceLedgerError::DigestMismatch)
            }
            SourceDigestLock::V1 | SourceDigestLock::V2 => {
                if &self.digest == expected {
                    Ok(())
                } else {
                    Err(SourceLedgerError::DigestMismatch)
                }
            }
        }
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
        let cell = SourceCell {
            id: id.clone(),
            boundary,
            payload,
            item,
        };
        match self.lock {
            SourceDigestLock::V2 => {
                let encoded = serde_json::to_vec(&cell)
                    .map_err(|error| SourceLedgerError::Serialize(error.to_string()))?;
                let encoded_size = self.encoded_size.saturating_add(encoded.len());
                if encoded_size > MAX_SOURCE_SNAPSHOT_BYTES {
                    return Err(SourceLedgerError::SnapshotTooLarge {
                        max_bytes: MAX_SOURCE_SNAPSHOT_BYTES,
                        actual_bytes: encoded_size,
                    });
                }
                self.hasher.update((encoded.len() as u64).to_le_bytes());
                self.hasher.update(&encoded);
                self.digest = digest_from_hasher(&self.hasher)?;
                self.encoded_size = encoded_size;
            }
            SourceDigestLock::V1 => {}
            SourceDigestLock::Unlocked => {}
        }
        self.cells.push(cell);
        self.last_raw_boundary = Some(raw_boundary);
        self.next_ordinal = self.next_ordinal.saturating_add(1);
        Ok(id)
    }

    fn recompute_locked_digest(&mut self) -> Result<(), SourceLedgerError> {
        match self.lock {
            SourceDigestLock::V1 => {
                self.digest = digest_cells(&self.cells)?;
            }
            SourceDigestLock::V2 => {
                let (digest, hasher, encoded_size) = digest_v2(&self.cells)?;
                self.digest = digest;
                self.hasher = hasher;
                self.encoded_size = encoded_size;
            }
            SourceDigestLock::Unlocked => {}
        }
        Ok(())
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

fn v2_hasher() -> Sha256 {
    let mut hasher = Sha256::new();
    hasher.update(SOURCE_DIGEST_V2_DOMAIN);
    hasher
}

fn digest_from_hasher(hasher: &Sha256) -> Result<RecordDigest, SourceLedgerError> {
    RecordDigest::parse(format!("{:x}", hasher.clone().finalize()))
        .map_err(|error| SourceLedgerError::Serialize(error.to_string()))
}

fn digest_v2(
    cells: &[SourceCell],
) -> Result<(RecordDigest, Sha256, usize), SourceLedgerError> {
    let mut hasher = v2_hasher();
    let mut encoded_size = 0usize;
    for cell in cells {
        let encoded = serde_json::to_vec(cell)
            .map_err(|error| SourceLedgerError::Serialize(error.to_string()))?;
        encoded_size = encoded_size.saturating_add(encoded.len());
        hasher.update((encoded.len() as u64).to_le_bytes());
        hasher.update(&encoded);
    }
    if encoded_size > MAX_SOURCE_SNAPSHOT_BYTES {
        return Err(SourceLedgerError::SnapshotTooLarge {
            max_bytes: MAX_SOURCE_SNAPSHOT_BYTES,
            actual_bytes: encoded_size,
        });
    }
    let digest = digest_from_hasher(&hasher)?;
    Ok((digest, hasher, encoded_size))
}
