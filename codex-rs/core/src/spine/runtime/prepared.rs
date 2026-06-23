use codex_protocol::models::ResponseItem;

use super::CompletedToolCall;
use super::SpineRootCompactResult;
use crate::spine::model::MemRecord;
use crate::spine::parser::ParserPreparedState;
use crate::spine::parser::ParserPublicationPlan;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SpineCommitKind {
    Open { open_request_index: usize },
    Close,
    CloseThenOpen { open_index: usize },
}

#[derive(Debug)]
pub(crate) struct SpinePreparedCommit {
    pub(super) kind: SpineCommitKind,
    pub(super) publication_plan: Option<ParserPublicationPlan>,
    pub(super) final_parse_stack: Option<ParserPreparedState>,
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
pub(crate) struct SpineCommitPublication<T> {
    application: Option<SpinePreparedCommitApplication>,
    history_update: Option<T>,
    defer_tree_update_until_raw_output: bool,
}

#[derive(Debug)]
pub(crate) struct SpinePreparedRootCompact {
    pub(super) result: SpineRootCompactResult,
    pub(super) final_parse_stack: ParserPreparedState,
}

#[derive(Debug)]
pub(crate) struct SpinePreparedRootCompactInstall {
    prepared: SpinePreparedRootCompact,
}

impl SpinePreparedRootCompact {
    pub(crate) fn result(&self) -> &SpineRootCompactResult {
        &self.result
    }

    pub(crate) fn into_install(self) -> SpinePreparedRootCompactInstall {
        SpinePreparedRootCompactInstall { prepared: self }
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
    pub(crate) fn publication_plan(&self) -> Option<&ParserPublicationPlan> {
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

impl<T> SpineCommitPublication<T> {
    pub(super) fn new(
        application: Option<SpinePreparedCommitApplication>,
        history_update: Option<T>,
    ) -> Self {
        let defer_tree_update_until_raw_output = application
            .as_ref()
            .is_some_and(SpinePreparedCommitApplication::defer_tree_update_until_raw_output);
        Self {
            application,
            history_update,
            defer_tree_update_until_raw_output,
        }
    }

    pub(crate) fn defer_tree_update_until_raw_output(&self) -> bool {
        self.defer_tree_update_until_raw_output
    }

    pub(crate) fn take_history_update(&mut self) -> Option<T> {
        self.history_update.take()
    }

    pub(super) fn application(&self) -> Option<&SpinePreparedCommitApplication> {
        self.application.as_ref()
    }

    pub(super) fn into_application(self) -> Option<SpinePreparedCommitApplication> {
        self.application
    }
}

impl SpinePreparedRootCompactInstall {
    pub(crate) fn result(&self) -> &SpineRootCompactResult {
        self.prepared.result()
    }

    pub(super) fn into_prepared(self) -> SpinePreparedRootCompact {
        self.prepared
    }
}
