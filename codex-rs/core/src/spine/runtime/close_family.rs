use codex_protocol::models::ResponseItem;

use super::SpineCommitKind;
use super::SpineError;
use crate::spine::archive::StagedArchiveWrite;
use crate::spine::model::MemRecord;
use crate::spine::model::MemoryRef;
use crate::spine::model::NodeId;
use crate::spine::model::SpineCommitKindMarker;
use crate::spine::model::SpineLedgerEvent;
use crate::spine::parse_stack::PreparedTaskTreeReduction;

pub(super) struct PreparedCloseCommit {
    pub(super) suffix_start: usize,
    pub(super) replacement: Vec<ResponseItem>,
    pub(super) mem: MemRecord,
    pub(super) memory_body: String,
    pub(super) archive_writes: Vec<StagedArchiveWrite>,
    pub(super) close_event: SpineLedgerEvent,
    pub(super) memory: MemoryRef,
    pub(super) task_tree_reduction: PreparedTaskTreeReduction,
}

pub(super) enum CloseFamilyAfterClose {
    None,
    Open { summary: String },
}

pub(super) struct CloseFamilyOpenPlan {
    pub(super) child: NodeId,
    pub(super) open_index_u64: u64,
    pub(super) summary: String,
    pub(super) event: SpineLedgerEvent,
}

pub(super) struct CloseFamilyPlan {
    pub(super) operation: &'static str,
    pub(super) missing_toolcall_error: &'static str,
    pub(super) event_count_underflow_error: &'static str,
    pub(super) toolcall_seq_overflow_error: &'static str,
    pub(super) marker_kind: SpineCommitKindMarker,
    pub(super) kind: SpineCommitKind,
    pub(super) toolcall_context_index: Option<usize>,
    pub(super) open: Option<CloseFamilyOpenPlan>,
}

pub(super) struct CloseFamilyTransaction<'a> {
    pub(super) mem: &'a MemRecord,
    pub(super) memory_body: &'a str,
    pub(super) archive_writes: &'a [StagedArchiveWrite],
    pub(super) events: Vec<SpineLedgerEvent>,
    pub(super) marker_kind: SpineCommitKindMarker,
    pub(super) close_event: &'a SpineLedgerEvent,
    pub(super) event_count: u64,
}

pub(super) enum CloseFamilyTransactionError {
    PreparedSideEffect(SpineError),
    CommitProof(SpineError),
}
