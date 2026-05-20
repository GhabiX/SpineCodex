use crate::spine::fast_fail::RuntimeFastFailError;
use crate::spine::fast_fail::mem_install_body_error;
use crate::spine::fast_fail::validate_mem_install_metadata;
use crate::spine::ids::NodeId;
use crate::spine::mem_install::MemoryBodyRef;
use crate::spine::mem_install::MemorySectionId;
use codex_protocol::models::ResponseItem;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::PathBuf;

use super::COMPACT_INDEX_FILE;
use super::MEM_INSTALL_COMMITTED_SCHEMA_VERSION;
use super::NOTE_EVIDENCE_COMMITTED_SCHEMA_VERSION;
use super::SpineOperation;
use super::SpineSidecarStore;
use super::SpineStoreError;
use super::jsonl_ledger::SequencedLedgerEvent;
use super::validate_note_evidence_metadata;

#[derive(Clone, Debug)]
pub(crate) struct CompactAttemptRecord {
    pub(crate) compact_id: String,
    pub(crate) node_id: NodeId,
    pub(crate) op: SpineOperation,
    pub(crate) cut_ordinal: u64,
    pub(crate) fold_end_ordinal: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct CompactStartedRecord {
    pub(crate) attempt: CompactAttemptRecord,
    pub(crate) strategy: String,
    pub(crate) rollout: String,
}

#[derive(Clone, Debug)]
pub(crate) struct MemInstallCommittedRecord {
    pub(crate) attempt: CompactAttemptRecord,
    pub(crate) body_ref: MemoryBodyRef,
    pub(crate) projection_ref: String,
    pub(crate) source_rollout_ref: String,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NotePlacement {
    BeforeMem,
    AfterMem,
}

#[derive(Clone, Debug)]
pub(crate) struct NoteEvidenceCommittedRecord {
    pub(crate) compact_id: String,
    pub(crate) placement: NotePlacement,
    pub(crate) kind: String,
    pub(crate) items: Vec<ResponseItem>,
    pub(crate) projection_ref: String,
    pub(crate) source_rollout_ref: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CommittedMemInstall {
    pub(crate) compact_id: String,
    pub(crate) node_id: NodeId,
    pub(crate) op: SpineOperation,
    pub(crate) cut_ordinal: u64,
    pub(crate) fold_end_ordinal: u64,
    pub(crate) body_ref: MemoryBodyRef,
    pub(crate) projection_ref: String,
    pub(crate) source_rollout_ref: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CommittedNoteEvidence {
    pub(crate) compact_id: String,
    pub(crate) placement: NotePlacement,
    pub(crate) kind: String,
    pub(crate) items_hash: String,
    pub(crate) items: Vec<ResponseItem>,
    pub(crate) projection_ref: String,
    pub(crate) source_rollout_ref: String,
}

#[derive(Clone, Debug)]
pub(crate) struct CompactTerminalRecord {
    pub(crate) attempt: CompactAttemptRecord,
    pub(crate) strategy: String,
    pub(crate) error: String,
}

#[derive(Debug)]
pub(super) struct CompactAttemptState {
    pub(super) node_id: String,
    pub(super) op: SpineOperation,
    pub(super) cut_ordinal: u64,
    pub(super) fold_end_ordinal: u64,
    pub(super) rollout: String,
    pub(super) terminal: Option<&'static str>,
    pub(super) mem_install_committed: bool,
    pub(super) root_note_evidence_committed: bool,
    pub(super) note_evidence_kinds: HashSet<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InstalledCompactSpan {
    pub(crate) compact_id: String,
    pub(crate) node_id: NodeId,
    pub(crate) op: SpineOperation,
    pub(crate) cut_ordinal: u64,
    pub(crate) fold_end_ordinal: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum CompactIndexEvent {
    CompactStarted {
        seq: u64,
        compact_id: String,
        node_id: String,
        op: SpineOperation,
        cut_ordinal: u64,
        fold_end_ordinal: u64,
        strategy: String,
        rollout: String,
    },
    MemInstallCommitted {
        seq: u64,
        schema_version: u32,
        compact_id: String,
        node_id: String,
        op: SpineOperation,
        cut_ordinal: u64,
        fold_end_ordinal: u64,
        memory_section_id: String,
        body_hash: String,
        storage_ref: String,
        projection_ref: String,
        source_rollout_ref: String,
    },
    NoteEvidenceCommitted {
        seq: u64,
        schema_version: u32,
        compact_id: String,
        placement: NotePlacement,
        kind: String,
        items_hash: String,
        items: Vec<ResponseItem>,
        projection_ref: String,
        source_rollout_ref: String,
    },
    CompactFailed {
        seq: u64,
        compact_id: String,
        node_id: String,
        op: SpineOperation,
        cut_ordinal: u64,
        fold_end_ordinal: u64,
        strategy: String,
        error: String,
    },
    CompactInterrupted {
        seq: u64,
        compact_id: String,
        node_id: String,
        op: SpineOperation,
        cut_ordinal: u64,
        fold_end_ordinal: u64,
        strategy: String,
        error: String,
    },
}

impl SequencedLedgerEvent for CompactIndexEvent {
    fn seq(&self) -> u64 {
        match self {
            CompactIndexEvent::CompactStarted { seq, .. }
            | CompactIndexEvent::MemInstallCommitted { seq, .. }
            | CompactIndexEvent::NoteEvidenceCommitted { seq, .. }
            | CompactIndexEvent::CompactFailed { seq, .. }
            | CompactIndexEvent::CompactInterrupted { seq, .. } => *seq,
        }
    }

    fn set_seq(&mut self, next_seq: u64) {
        match self {
            CompactIndexEvent::CompactStarted { seq, .. }
            | CompactIndexEvent::MemInstallCommitted { seq, .. }
            | CompactIndexEvent::NoteEvidenceCommitted { seq, .. }
            | CompactIndexEvent::CompactFailed { seq, .. }
            | CompactIndexEvent::CompactInterrupted { seq, .. } => *seq = next_seq,
        }
    }
}

pub(super) fn record_compact_terminal(
    attempts: &mut HashMap<String, CompactAttemptState>,
    compact_id: String,
    node_id: String,
    op: SpineOperation,
    cut_ordinal: u64,
    fold_end_ordinal: u64,
    terminal: &'static str,
) -> Result<(), SpineStoreError> {
    let Some(attempt) = attempts.get_mut(&compact_id) else {
        return Err(SpineStoreError::InvalidLedger(format!(
            "compact.index.jsonl has {terminal} without matching compact_started for {compact_id}"
        )));
    };
    if attempt.mem_install_committed {
        return Err(RuntimeFastFailError::MemInstallInvalidTerminalAfterCommit {
            compact_id,
            terminal,
        }
        .into());
    }
    if attempt.terminal.is_some() {
        return Err(SpineStoreError::InvalidLedger(format!(
            "compact.index.jsonl has duplicate terminal event for {compact_id}"
        )));
    }
    if attempt.node_id != node_id
        || attempt.op != op
        || attempt.cut_ordinal != cut_ordinal
        || attempt.fold_end_ordinal != fold_end_ordinal
    {
        return Err(SpineStoreError::InvalidLedger(format!(
            "compact.index.jsonl {terminal} does not match compact_started for {compact_id}"
        )));
    }
    attempt.terminal = Some(terminal);
    Ok(())
}

pub(super) fn record_mem_install_committed(
    store: &SpineSidecarStore,
    attempts: &mut HashMap<String, CompactAttemptState>,
    _seq: u64,
    schema_version: u32,
    compact_id: String,
    node_id: String,
    op: SpineOperation,
    cut_ordinal: u64,
    fold_end_ordinal: u64,
    memory_section_id: String,
    body_hash: String,
    storage_ref: String,
    projection_ref: String,
    source_rollout_ref: String,
) -> Result<(), SpineStoreError> {
    if schema_version != MEM_INSTALL_COMMITTED_SCHEMA_VERSION {
        return Err(RuntimeFastFailError::MemInstallUnsupportedSchema {
            compact_id,
            schema_version,
        }
        .into());
    }

    let Some(attempt) = attempts.get_mut(&compact_id) else {
        return Err(RuntimeFastFailError::MemInstallMissingStarted { compact_id }.into());
    };
    if attempt.mem_install_committed {
        return Err(RuntimeFastFailError::MemInstallDuplicateCompactId { compact_id }.into());
    }
    if let Some(terminal) = attempt.terminal {
        return Err(RuntimeFastFailError::MemInstallCheckpointBeforeCommit {
            compact_id,
            terminal,
        }
        .into());
    }
    if attempt.node_id != node_id
        || attempt.op != op
        || attempt.cut_ordinal != cut_ordinal
        || attempt.fold_end_ordinal != fold_end_ordinal
    {
        return Err(RuntimeFastFailError::MemInstallSpanMismatch { compact_id }.into());
    }
    validate_mem_install_metadata(
        &compact_id,
        &projection_ref,
        &source_rollout_ref,
        attempt.rollout == source_rollout_ref,
    )?;
    if op == SpineOperation::Archive && !attempt.root_note_evidence_committed {
        return Err(SpineStoreError::InvalidLedger(format!(
            "compact.index.jsonl root archive MemInstall {compact_id} is missing NoteEvidenceCommitted"
        )));
    }
    let node_id = NodeId::parse(&node_id)?;
    let body_ref = MemoryBodyRef {
        section_id: MemorySectionId::parse(memory_section_id, storage_ref)
            .map_err(|err| mem_install_body_error(&compact_id, err))?,
        body_hash,
    };
    match store.verify_memory_body_ref(&node_id, &body_ref) {
        Ok(_) => {}
        Err(SpineStoreError::MemoryBody(err)) => {
            return Err(mem_install_body_error(&compact_id, err).into());
        }
        Err(err) => return Err(err),
    }
    attempt.mem_install_committed = true;
    Ok(())
}

pub(super) fn record_note_evidence_committed(
    attempts: &mut HashMap<String, CompactAttemptState>,
    _seq: u64,
    schema_version: u32,
    compact_id: String,
    placement: NotePlacement,
    kind: String,
    items_hash: String,
    items: Vec<ResponseItem>,
    projection_ref: String,
    source_rollout_ref: String,
) -> Result<(), SpineStoreError> {
    if schema_version != NOTE_EVIDENCE_COMMITTED_SCHEMA_VERSION {
        return Err(SpineStoreError::InvalidLedger(format!(
            "compact.index.jsonl NoteEvidenceCommitted {compact_id}/{kind} has unsupported schema_version {schema_version}"
        )));
    }
    let Some(attempt) = attempts.get_mut(&compact_id) else {
        return Err(SpineStoreError::InvalidLedger(format!(
            "compact.index.jsonl NoteEvidenceCommitted for {compact_id}/{kind} has no matching CompactStarted"
        )));
    };
    if let Some(terminal) = attempt.terminal {
        return Err(SpineStoreError::InvalidLedger(format!(
            "compact.index.jsonl NoteEvidenceCommitted for {compact_id}/{kind} follows {terminal}"
        )));
    }
    if attempt.mem_install_committed {
        return Err(SpineStoreError::InvalidLedger(format!(
            "compact.index.jsonl NoteEvidenceCommitted for {compact_id}/{kind} follows MemInstallCommitted"
        )));
    }
    validate_note_evidence_metadata(
        &compact_id,
        &kind,
        &projection_ref,
        &source_rollout_ref,
        attempt.rollout == source_rollout_ref,
    )?;
    if items.is_empty() {
        return Err(SpineStoreError::InvalidLedger(format!(
            "compact.index.jsonl NoteEvidenceCommitted for {compact_id}/{kind} has empty items"
        )));
    }
    if !attempt.note_evidence_kinds.insert(kind.clone()) {
        return Err(SpineStoreError::InvalidLedger(format!(
            "compact.index.jsonl has duplicate NoteEvidenceCommitted for {compact_id}/{kind}"
        )));
    }
    if placement == NotePlacement::BeforeMem && attempt.op != SpineOperation::Archive {
        return Err(SpineStoreError::InvalidLedger(format!(
            "compact.index.jsonl NoteEvidenceCommitted {compact_id}/{kind} uses before_mem placement for non-root {:?} compact",
            attempt.op
        )));
    }
    if placement == NotePlacement::BeforeMem {
        attempt.root_note_evidence_committed = true;
    }
    let actual_hash = note_evidence_items_hash(&items)?;
    if items_hash != actual_hash {
        return Err(SpineStoreError::InvalidLedger(format!(
            "compact.index.jsonl NoteEvidenceCommitted {compact_id}/{kind} items hash mismatch: expected {items_hash}, actual {actual_hash}"
        )));
    }
    Ok(())
}

pub(super) fn note_evidence_items_hash(items: &[ResponseItem]) -> Result<String, SpineStoreError> {
    let encoded = serde_json::to_string(items).map_err(|source| SpineStoreError::Json {
        path: PathBuf::from(COMPACT_INDEX_FILE),
        source,
    })?;
    Ok(format!("sha256:{:x}", Sha256::digest(encoded.as_bytes())))
}
