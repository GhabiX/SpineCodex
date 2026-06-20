use codex_protocol::models::ResponseItem;

use super::CompletedToolCall;
use super::SpineRootCompactResult;
use crate::spine::model::MemRecord;
use crate::spine::parse_stack::ParseStack;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SpineCommitKind {
    Open { open_request_index: usize },
    Close,
    CloseThenOpen { open_index: usize },
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HistoryPublicationPlan {
    pub(super) operation: &'static str,
    pub(super) suffix_start: usize,
    pub(super) replacement_prefix: Vec<ResponseItem>,
    pub(super) preserve_host_history_from: usize,
    pub(super) append_current_tool_response_if_missing: bool,
}

impl HistoryPublicationPlan {
    pub(crate) fn operation(&self) -> &'static str {
        self.operation
    }

    pub(crate) fn suffix_start(&self) -> usize {
        self.suffix_start
    }

    pub(crate) fn replacement_prefix(&self) -> &[ResponseItem] {
        &self.replacement_prefix
    }

    pub(crate) fn preserve_host_history_from(&self) -> usize {
        self.preserve_host_history_from
    }

    pub(crate) fn append_current_tool_response_if_missing(&self) -> bool {
        self.append_current_tool_response_if_missing
    }
}

#[derive(Debug)]
pub(crate) struct SpinePreparedCommit {
    pub(super) kind: SpineCommitKind,
    pub(super) publication_plan: Option<HistoryPublicationPlan>,
    pub(super) final_parse_stack: Option<ParseStack>,
    pub(super) completed_toolcall: Option<CompletedToolCall>,
    pub(super) toolcall_seq: Option<u64>,
    pub(super) raw_items: Vec<Option<ResponseItem>>,
    pub(super) mem_for_accounting: Option<MemRecord>,
}

#[derive(Debug)]
pub(crate) struct SpinePreparedRootCompact {
    pub(super) result: SpineRootCompactResult,
    pub(super) final_parse_stack: ParseStack,
}

impl SpinePreparedRootCompact {
    pub(crate) fn result(&self) -> &SpineRootCompactResult {
        &self.result
    }
}

impl SpinePreparedCommit {
    pub(crate) fn kind(&self) -> &SpineCommitKind {
        &self.kind
    }

    pub(crate) fn publication_plan(&self) -> Option<&HistoryPublicationPlan> {
        self.publication_plan.as_ref()
    }
}
