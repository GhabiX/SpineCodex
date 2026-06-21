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
    #[cfg(test)]
    pub(crate) fn operation(&self) -> &'static str {
        self.operation
    }

    #[cfg(test)]
    pub(crate) fn suffix_start(&self) -> usize {
        self.suffix_start
    }

    #[cfg(test)]
    pub(crate) fn replacement_prefix(&self) -> &[ResponseItem] {
        &self.replacement_prefix
    }

    #[cfg(test)]
    pub(crate) fn preserve_host_history_from(&self) -> usize {
        self.preserve_host_history_from
    }

    #[cfg(test)]
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
pub(crate) struct SpinePreparedCommitApplication {
    prepared: SpinePreparedCommit,
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
    pub(super) fn into_application(self) -> SpinePreparedCommitApplication {
        SpinePreparedCommitApplication { prepared: self }
    }

    #[cfg(test)]
    pub(crate) fn kind(&self) -> &SpineCommitKind {
        &self.kind
    }

    pub(crate) fn defer_tree_update_until_raw_output(&self) -> bool {
        matches!(
            self.kind,
            SpineCommitKind::Close | SpineCommitKind::CloseThenOpen { .. }
        )
    }

    pub(crate) fn validate_against_host_history(
        &self,
        call_id: &str,
        history_items: &[ResponseItem],
    ) -> Result<(), super::SpineError> {
        if let SpineCommitKind::Open { open_request_index } = self.kind
            && open_request_index > history_items.len()
        {
            return Err(super::SpineError::Invariant(format!(
                "spine.open request index {open_request_index} exceeds history length {} for call_id={call_id}",
                history_items.len()
            )));
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn publication_plan(&self) -> Option<&HistoryPublicationPlan> {
        self.publication_plan.as_ref()
    }
}

impl SpinePreparedCommitApplication {
    pub(crate) fn defer_tree_update_until_raw_output(&self) -> bool {
        self.prepared.defer_tree_update_until_raw_output()
    }

    pub(crate) fn validate_against_host_history(
        &self,
        call_id: &str,
        history_items: &[ResponseItem],
    ) -> Result<(), super::SpineError> {
        self.prepared
            .validate_against_host_history(call_id, history_items)
    }

    pub(crate) fn as_prepared_commit(&self) -> &SpinePreparedCommit {
        &self.prepared
    }

    pub(super) fn into_prepared_commit(self) -> SpinePreparedCommit {
        self.prepared
    }
}
