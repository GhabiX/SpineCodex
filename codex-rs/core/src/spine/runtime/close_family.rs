use codex_protocol::models::ResponseItem;

use super::SpineCommitKind;
use super::SpineError;
use super::pending::CompletedToolCall;
use crate::spine::archive::StagedArchiveWrite;
use crate::spine::lexer::LexedTokenBatch;
use crate::spine::model::MemRecord;
use crate::spine::model::MemoryRef;
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

pub(super) struct CloseFamilyPlan {
    operation: &'static str,
    missing_toolcall_error: &'static str,
    event_count_underflow_error: &'static str,
    toolcall_seq_overflow_error: &'static str,
    marker_kind: SpineCommitKindMarker,
    kind: SpineCommitKind,
    toolcall_context_index: Option<usize>,
    open: Option<LexedTokenBatch>,
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

impl CloseFamilyPlan {
    pub(super) fn close() -> Self {
        Self {
            operation: "spine.close",
            missing_toolcall_error: "spine.close commit requires completed toolcall evidence",
            event_count_underflow_error: "spine close event count underflow",
            toolcall_seq_overflow_error: "spine.close toolcall seq overflow",
            marker_kind: SpineCommitKindMarker::Close,
            kind: SpineCommitKind::Close,
            toolcall_context_index: None,
            open: None,
        }
    }

    pub(super) fn next(open_index: usize, open_lexed: LexedTokenBatch) -> Self {
        Self {
            operation: "spine.next",
            missing_toolcall_error: "spine.next commit requires completed toolcall evidence",
            event_count_underflow_error: "spine next event count underflow",
            toolcall_seq_overflow_error: "spine.next toolcall seq overflow",
            marker_kind: SpineCommitKindMarker::CloseThenOpen,
            kind: SpineCommitKind::CloseThenOpen { open_index },
            toolcall_context_index: Some(open_index),
            open: Some(open_lexed),
        }
    }

    pub(super) fn operation(&self) -> &'static str {
        self.operation
    }

    pub(super) fn kind(&self) -> SpineCommitKind {
        self.kind.clone()
    }

    pub(super) fn marker_kind(&self) -> SpineCommitKindMarker {
        self.marker_kind
    }

    pub(super) fn open_lexed(&self) -> Option<&LexedTokenBatch> {
        self.open.as_ref()
    }

    pub(super) fn append_open_events(&self, events: &mut Vec<SpineLedgerEvent>) {
        if let Some(open) = self.open_lexed() {
            events.extend(open.events().iter().cloned());
        }
    }

    pub(super) fn require_completed_toolcall(
        &self,
        completed_toolcall: Option<CompletedToolCall>,
    ) -> Result<CompletedToolCall, SpineError> {
        completed_toolcall
            .ok_or_else(|| SpineError::InvalidEvent(self.missing_toolcall_error.to_string()))
    }

    pub(super) fn toolcall_context_index(
        &self,
        prepared: &PreparedCloseCommit,
    ) -> Result<usize, SpineError> {
        if let Some(index) = self.toolcall_context_index {
            return Ok(index);
        }
        prepared
            .suffix_start
            .checked_add(prepared.replacement.len())
            .ok_or_else(|| {
                SpineError::InvalidEvent("spine.close toolcall context index overflow".to_string())
            })
    }

    pub(super) fn toolcall_seq(
        &self,
        next_event_seq: u64,
        event_count: u64,
    ) -> Result<u64, SpineError> {
        next_event_seq
            .checked_add(event_count.checked_sub(1).ok_or_else(|| {
                SpineError::InvalidEvent(self.event_count_underflow_error.to_string())
            })?)
            .ok_or_else(|| SpineError::InvalidEvent(self.toolcall_seq_overflow_error.to_string()))
    }
}
