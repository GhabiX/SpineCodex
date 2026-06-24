use codex_protocol::models::ResponseItem;

use super::CompletedToolCall;
use super::SpineRootCompactResult;
use crate::spine::model::MemRecord;
use crate::spine::parser::ParserCommitInstall;
use crate::spine::parser::ParserPublicationPlan;
use crate::spine::parser::ParserRootCompactInstall;

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
    pub(super) parser_install: Option<ParserCommitInstall>,
    pub(super) completed_toolcall: Option<CompletedToolCall>,
    pub(super) toolcall_seq: Option<u64>,
    pub(super) raw_items: Vec<Option<ResponseItem>>,
    pub(super) mem_for_accounting: Option<MemRecord>,
}

#[derive(Debug)]
pub(crate) struct SpinePreparedCommitInstall {
    prepared: SpinePreparedCommit,
}

#[derive(Debug)]
pub(crate) struct SpineCommitPublication<T> {
    install: Option<SpinePreparedCommitInstall>,
    history_update: Option<T>,
}

#[derive(Debug)]
pub(crate) struct SpinePreparedRootCompact {
    pub(super) result: SpineRootCompactResult,
    pub(super) parser_install: ParserRootCompactInstall,
}

impl SpinePreparedRootCompact {
    pub(crate) fn result(&self) -> &SpineRootCompactResult {
        &self.result
    }
}

impl SpinePreparedCommit {
    pub(super) fn into_install(self) -> SpinePreparedCommitInstall {
        SpinePreparedCommitInstall { prepared: self }
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

impl SpinePreparedCommitInstall {
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
        install: Option<SpinePreparedCommitInstall>,
        history_update: Option<T>,
    ) -> Self {
        Self {
            install,
            history_update,
        }
    }

    pub(crate) fn defer_tree_update_until_raw_output(&self) -> bool {
        self.install
            .as_ref()
            .is_some_and(SpinePreparedCommitInstall::defer_tree_update_until_raw_output)
    }

    pub(crate) fn take_history_update(&mut self) -> Option<T> {
        self.history_update.take()
    }

    pub(super) fn install(&self) -> Option<&SpinePreparedCommitInstall> {
        self.install.as_ref()
    }

    pub(super) fn into_install(self) -> Option<SpinePreparedCommitInstall> {
        self.install
    }
}
