use codex_protocol::models::ResponseItem;

use super::CompletedToolCall;
use super::SpineRootCompactResult;
use super::support::HostHistoryLens;
use crate::spine::model::MemRecord;
use crate::spine::model::ToolCallSegmentKind;
use crate::spine::model::TrimProjection;
use crate::spine::parser::ParserCommitInstall;
use crate::spine::parser::ParserPublicationPlan;
use crate::spine::parser::ParserPublicationToolcallSegment;
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
    pre_apply_history_update: Option<T>,
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

    pub(crate) fn variable_context(&self) -> &[ResponseItem] {
        self.result.variable_context()
    }

    #[cfg(test)]
    pub(crate) fn clone_publication_result_for_test(&self) -> SpineRootCompactResult {
        self.result.clone()
    }

    pub(crate) fn validate_published_variable_context_len(
        &self,
        published_variable_context_len: usize,
    ) -> Result<(), super::SpineError> {
        let publication_variable_context_len = self.variable_context().len();
        if publication_variable_context_len != published_variable_context_len {
            return Err(super::SpineError::InvalidStore(format!(
                "spine root compact publication variable context length {publication_variable_context_len} does not match published variable context length {published_variable_context_len}"
            )));
        }
        Ok(())
    }

    pub(super) fn install_parser_state(self, install: impl FnOnce(ParserRootCompactInstall)) {
        install(self.parser_install);
    }

    #[cfg(test)]
    pub(super) fn install_for_direct_result(
        self,
        install: impl FnOnce(ParserRootCompactInstall),
    ) -> SpineRootCompactResult {
        install(self.parser_install);
        self.result
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

    pub(super) fn into_kind_and_install(self) -> (SpineCommitKind, SpinePreparedCommitInstall) {
        let kind = self.kind.clone();
        (kind, SpinePreparedCommitInstall { prepared: self })
    }

    pub(super) fn into_install(self) -> SpinePreparedCommitInstall {
        self.into_kind_and_install().1
    }

    #[cfg(test)]
    pub(crate) fn into_kind_and_install_for_test(
        self,
    ) -> (SpineCommitKind, SpinePreparedCommitInstall) {
        self.into_kind_and_install()
    }
}

impl SpinePreparedCommitInstall {
    pub(crate) fn defer_tree_update_until_raw_output(&self) -> bool {
        matches!(
            self.prepared.kind,
            SpineCommitKind::Close | SpineCommitKind::CloseThenOpen { .. }
        )
    }

    pub(crate) fn validate_against_host_history(
        &self,
        call_id: &str,
        history_items: &[ResponseItem],
    ) -> Result<(), super::SpineError> {
        if let SpineCommitKind::Open { open_request_index } = &self.prepared.kind
            && *open_request_index > history_items.len()
        {
            return Err(super::SpineError::Invariant(format!(
                "spine.open request index {open_request_index} exceeds history length {} for call_id={call_id}",
                history_items.len()
            )));
        }
        Ok(())
    }

    pub(crate) fn apply_variable_context_publication_update<T, F>(
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
        let Some(plan) = self.prepared.publication_plan.as_ref() else {
            return Ok(None);
        };
        let build_update = build_update.take().ok_or_else(|| {
            super::SpineError::Invariant(
                "spine prepared publication update builder was already consumed".to_string(),
            )
        })?;
        let host_history = HostHistoryLens::new(history_items);
        let host_suffix_start =
            host_history.full_index_for_mutable_boundary(plan.suffix_start())?;
        let host_preserve_history_from =
            host_history.full_index_for_mutable_boundary(plan.preserve_host_history_from())?;
        validate_publication_boundaries_do_not_split_toolcall(
            plan.atomic_mutable_context_segments(),
            host_suffix_start,
            host_preserve_history_from,
            history_items,
        )?;
        let update = plan.history_update_with_host_boundaries(
            call_id,
            tool_resp_item,
            tool_resp_already_recorded,
            host_suffix_start,
            host_preserve_history_from,
            history_items,
        )?;
        Ok(update.map(|update| update.into_history_update(call_id, build_update)))
    }

    pub(super) fn full_variable_context_host_history_update<T>(
        &self,
        call_id: &str,
        operation: &'static str,
        raw_items: &[Option<ResponseItem>],
        trim_projection: &TrimProjection,
        history_items: &[ResponseItem],
        build_update: impl FnOnce(&str, &'static str, usize, Vec<ResponseItem>, Vec<ResponseItem>) -> T,
    ) -> Result<Option<T>, super::SpineError> {
        let Some(parser_install) = self.prepared.parser_install.as_ref() else {
            return Ok(None);
        };
        parser_install.full_variable_context_host_history_update(
            call_id,
            operation,
            raw_items,
            trim_projection,
            history_items,
            build_update,
        )
    }

    pub(super) fn trim_candidate_inputs(
        &self,
    ) -> Option<(&CompletedToolCall, u64, &[Option<ResponseItem>])> {
        Some((
            self.prepared.completed_toolcall.as_ref()?,
            self.prepared.toolcall_seq?,
            self.prepared.raw_items.as_slice(),
        ))
    }

    pub(super) fn mem_for_accounting(&self) -> Option<&MemRecord> {
        self.prepared.mem_for_accounting.as_ref()
    }

    pub(super) fn install_parser_state(
        self,
        install: impl FnOnce(ParserCommitInstall),
    ) -> Option<CompletedToolCall> {
        if let Some(parser_install) = self.prepared.parser_install {
            install(parser_install);
        }
        self.prepared.completed_toolcall
    }
}

fn validate_publication_boundaries_do_not_split_toolcall(
    atomic_mutable_context_segments: &[ParserPublicationToolcallSegment],
    host_suffix_start: usize,
    host_preserve_history_from: usize,
    history_items: &[ResponseItem],
) -> Result<(), super::SpineError> {
    if atomic_mutable_context_segments.is_empty() {
        return Ok(());
    }
    let host_history = HostHistoryLens::new(history_items);
    let mut full_start = usize::MAX;
    let mut full_end = 0usize;
    for segment in atomic_mutable_context_segments {
        match segment.kind {
            ToolCallSegmentKind::Request => {
                let full_index =
                    host_history.full_index_for_mutable_index(segment.mutable_context_index)?;
                full_start = full_start.min(full_index);
                full_end = full_end.max(full_index.checked_add(1).ok_or_else(|| {
                    super::SpineError::InvalidEvent("toolcall full host range overflow".to_string())
                })?);
            }
            ToolCallSegmentKind::Response => {
                let full_boundary =
                    host_history.full_index_for_mutable_boundary(segment.mutable_context_index)?;
                full_start = full_start.min(full_boundary);
                let response_end = if full_boundary == history_items.len() {
                    full_boundary
                } else {
                    full_boundary.checked_add(1).ok_or_else(|| {
                        super::SpineError::InvalidEvent(
                            "toolcall full host range overflow".to_string(),
                        )
                    })?
                };
                full_end = full_end.max(response_end);
            }
        }
    }
    for boundary in [host_suffix_start, host_preserve_history_from] {
        if full_start < boundary && boundary < full_end {
            return Err(super::SpineError::Invariant(format!(
                "spine publication boundary {boundary} splits completed toolcall full host range [{full_start}..{full_end})"
            )));
        }
    }
    Ok(())
}

impl<T> SpineCommitPublication<T> {
    pub(super) fn new(
        install: Option<SpinePreparedCommitInstall>,
        pre_apply_history_update: Option<T>,
    ) -> Self {
        Self {
            install,
            pre_apply_history_update,
        }
    }

    pub(crate) fn defer_tree_update_until_raw_output(&self) -> bool {
        self.install
            .as_ref()
            .is_some_and(SpinePreparedCommitInstall::defer_tree_update_until_raw_output)
    }

    pub(crate) fn take_pre_apply_history_update(&mut self) -> Option<T> {
        self.pre_apply_history_update.take()
    }

    pub(super) fn apply_install_side_effects(
        &self,
        apply: impl FnOnce(&SpinePreparedCommitInstall) -> Result<(), super::SpineError>,
    ) -> Result<(), super::SpineError> {
        if let Some(install) = self.install.as_ref() {
            apply(install)?;
        }
        Ok(())
    }

    pub(super) fn into_install(self) -> Option<SpinePreparedCommitInstall> {
        self.install
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::models::ContentItem;
    use codex_protocol::models::FunctionCallOutputPayload;

    fn developer_fixed_prefix_item() -> ResponseItem {
        ResponseItem::Message {
            id: None,
            role: "developer".to_string(),
            content: vec![ContentItem::InputText {
                text: "fixed developer prefix".to_string(),
            }],
            phase: None,
        }
    }

    fn function_call(call_id: &str) -> ResponseItem {
        ResponseItem::FunctionCall {
            id: None,
            name: "test_tool".to_string(),
            namespace: None,
            arguments: "{}".to_string(),
            call_id: call_id.to_string(),
        }
    }

    fn function_output(call_id: &str) -> ResponseItem {
        ResponseItem::FunctionCallOutput {
            call_id: call_id.to_string(),
            output: FunctionCallOutputPayload::from_text("ok".to_string()),
        }
    }

    fn custom_tool_call(call_id: &str) -> ResponseItem {
        ResponseItem::CustomToolCall {
            id: None,
            status: None,
            call_id: call_id.to_string(),
            name: "custom_tool".to_string(),
            input: "input".to_string(),
        }
    }

    fn custom_tool_output(call_id: &str) -> ResponseItem {
        ResponseItem::CustomToolCallOutput {
            call_id: call_id.to_string(),
            name: Some("custom_tool".to_string()),
            output: FunctionCallOutputPayload::from_text("ok".to_string()),
        }
    }

    fn completed_toolcall_segments() -> Vec<ParserPublicationToolcallSegment> {
        vec![
            ParserPublicationToolcallSegment {
                kind: ToolCallSegmentKind::Request,
                mutable_context_index: 0,
            },
            ParserPublicationToolcallSegment {
                kind: ToolCallSegmentKind::Response,
                mutable_context_index: 1,
            },
        ]
    }

    #[test]
    fn publication_rejects_boundary_inside_function_toolcall() {
        let history_items = vec![
            developer_fixed_prefix_item(),
            function_call("function-call"),
            function_output("function-call"),
        ];
        let err = validate_publication_boundaries_do_not_split_toolcall(
            &completed_toolcall_segments(),
            2,
            history_items.len(),
            &history_items,
        )
        .expect_err("boundary between request and response must be rejected");
        assert!(
            err.to_string().contains("splits completed toolcall"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn publication_rejects_boundary_inside_custom_toolcall() {
        let history_items = vec![
            developer_fixed_prefix_item(),
            custom_tool_call("custom-call"),
            custom_tool_output("custom-call"),
        ];
        let err = validate_publication_boundaries_do_not_split_toolcall(
            &completed_toolcall_segments(),
            2,
            history_items.len(),
            &history_items,
        )
        .expect_err("boundary between request and response must be rejected");
        assert!(
            err.to_string().contains("splits completed toolcall"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn publication_accepts_boundaries_at_toolcall_edges() {
        let history_items = vec![
            developer_fixed_prefix_item(),
            custom_tool_call("custom-call"),
            custom_tool_output("custom-call"),
        ];
        validate_publication_boundaries_do_not_split_toolcall(
            &completed_toolcall_segments(),
            1,
            history_items.len(),
            &history_items,
        )
        .expect("boundary at toolcall start is valid");
        validate_publication_boundaries_do_not_split_toolcall(
            &completed_toolcall_segments(),
            0,
            3,
            &history_items,
        )
        .expect("boundary at toolcall end is valid");
    }
}
