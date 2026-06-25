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
    kind: SpineCommitKind,
    publication_plan: Option<ParserPublicationPlan>,
    parser_install: Option<ParserCommitInstall>,
    completed_toolcall: Option<CompletedToolCall>,
    toolcall_seq: Option<u64>,
    raw_items: Vec<Option<ResponseItem>>,
    mem_for_accounting: Option<MemRecord>,
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
    result: SpineRootCompactResult,
    parser_install: ParserRootCompactInstall,
}

impl SpinePreparedRootCompact {
    pub(super) fn new(
        result: SpineRootCompactResult,
        parser_install: ParserRootCompactInstall,
    ) -> Self {
        Self {
            result,
            parser_install,
        }
    }

    pub(crate) fn publication_history(&self) -> &[ResponseItem] {
        &self.result.materialized
    }

    #[cfg(test)]
    pub(crate) fn clone_publication_result_for_test(&self) -> SpineRootCompactResult {
        self.result.clone()
    }

    pub(crate) fn validate_published_history_len(
        &self,
        published_history_len: usize,
    ) -> Result<(), super::SpineError> {
        let current_open_index = self.result.materialized.len();
        if current_open_index != published_history_len {
            return Err(super::SpineError::InvalidStore(format!(
                "spine root compact open index {current_open_index} does not match materialized history length {published_history_len}"
            )));
        }
        Ok(())
    }

    pub(super) fn into_parser_install(self) -> ParserRootCompactInstall {
        self.into_publication_result_and_parser_install().1
    }

    pub(super) fn into_publication_result_and_parser_install(
        self,
    ) -> (SpineRootCompactResult, ParserRootCompactInstall) {
        (self.result, self.parser_install)
    }
}

impl SpinePreparedCommit {
    pub(super) fn installed_open(kind: SpineCommitKind) -> Self {
        Self {
            kind,
            publication_plan: None,
            parser_install: None,
            completed_toolcall: None,
            toolcall_seq: None,
            raw_items: Vec::new(),
            mem_for_accounting: None,
        }
    }

    pub(super) fn open_with_toolcall(
        kind: SpineCommitKind,
        parser_install: ParserCommitInstall,
        completed_toolcall: CompletedToolCall,
        toolcall_seq: u64,
        raw_items: Vec<Option<ResponseItem>>,
    ) -> Self {
        Self {
            kind,
            publication_plan: None,
            parser_install: Some(parser_install),
            completed_toolcall: Some(completed_toolcall),
            toolcall_seq: Some(toolcall_seq),
            raw_items,
            mem_for_accounting: None,
        }
    }

    pub(super) fn close_family(
        kind: SpineCommitKind,
        publication_plan: ParserPublicationPlan,
        parser_install: ParserCommitInstall,
        completed_toolcall: CompletedToolCall,
        toolcall_seq: u64,
        raw_items: Vec<Option<ResponseItem>>,
        mem_for_accounting: MemRecord,
    ) -> Self {
        Self {
            kind,
            publication_plan: Some(publication_plan),
            parser_install: Some(parser_install),
            completed_toolcall: Some(completed_toolcall),
            toolcall_seq: Some(toolcall_seq),
            raw_items,
            mem_for_accounting: Some(mem_for_accounting),
        }
    }

    pub(super) fn into_install(self) -> SpinePreparedCommitInstall {
        SpinePreparedCommitInstall { prepared: self }
    }

    #[cfg(test)]
    pub(crate) fn into_install_for_test(self) -> SpinePreparedCommitInstall {
        self.into_install()
    }

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

    pub(crate) fn apply_publication_history_update<T, F>(
        &self,
        call_id: &str,
        tool_resp_item: &ResponseItem,
        tool_resp_already_recorded: bool,
        history_items: &[ResponseItem],
        build_update: &mut Option<F>,
    ) -> Result<Option<T>, super::SpineError>
    where
        F: FnOnce(&str, &'static str, usize, Vec<ResponseItem>, Vec<ResponseItem>) -> T,
    {
        let Some(plan) = self.publication_plan.as_ref() else {
            return Ok(None);
        };
        let build_update = build_update.take().ok_or_else(|| {
            super::SpineError::Invariant(
                "spine prepared publication update builder was already consumed".to_string(),
            )
        })?;
        let update = plan.history_update(
            call_id,
            tool_resp_item,
            tool_resp_already_recorded,
            history_items,
        )?;
        Ok(update.map(|update| update.into_history_update(call_id, build_update)))
    }

    pub(super) fn parser_install(&self) -> Option<&ParserCommitInstall> {
        self.parser_install.as_ref()
    }

    pub(super) fn trim_candidate_inputs(
        &self,
    ) -> Option<(&CompletedToolCall, u64, &[Option<ResponseItem>])> {
        Some((
            self.completed_toolcall.as_ref()?,
            self.toolcall_seq?,
            self.raw_items.as_slice(),
        ))
    }

    pub(super) fn mem_for_accounting(&self) -> Option<&MemRecord> {
        self.mem_for_accounting.as_ref()
    }

    pub(super) fn into_install_parts(
        self,
    ) -> (Option<ParserCommitInstall>, Option<CompletedToolCall>) {
        (self.parser_install, self.completed_toolcall)
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

    pub(crate) fn apply_publication_history_update<T, F>(
        &self,
        call_id: &str,
        tool_resp_item: &ResponseItem,
        tool_resp_already_recorded: bool,
        history_items: &[ResponseItem],
        build_update: &mut Option<F>,
    ) -> Result<Option<T>, super::SpineError>
    where
        F: FnOnce(&str, &'static str, usize, Vec<ResponseItem>, Vec<ResponseItem>) -> T,
    {
        self.prepared.apply_publication_history_update(
            call_id,
            tool_resp_item,
            tool_resp_already_recorded,
            history_items,
            build_update,
        )
    }

    pub(super) fn parser_install(&self) -> Option<&ParserCommitInstall> {
        self.prepared.parser_install()
    }

    pub(super) fn trim_candidate_inputs(
        &self,
    ) -> Option<(&CompletedToolCall, u64, &[Option<ResponseItem>])> {
        self.prepared.trim_candidate_inputs()
    }

    pub(super) fn mem_for_accounting(&self) -> Option<&MemRecord> {
        self.prepared.mem_for_accounting()
    }

    pub(super) fn into_install_parts(
        self,
    ) -> (Option<ParserCommitInstall>, Option<CompletedToolCall>) {
        self.prepared.into_install_parts()
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

    pub(crate) fn take_pre_apply_history_update(&mut self) -> Option<T> {
        self.history_update.take()
    }

    pub(super) fn install(&self) -> Option<&SpinePreparedCommitInstall> {
        self.install.as_ref()
    }

    pub(super) fn into_install(self) -> Option<SpinePreparedCommitInstall> {
        self.install
    }
}
