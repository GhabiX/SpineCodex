use codex_protocol::models::ResponseItem;

use super::super::runtime;
use super::super::runtime::SpineError;

pub(crate) struct ToolCallEvidence<'a> {
    kind: ToolCallEvidenceKind<'a>,
}

enum ToolCallEvidenceKind<'a> {
    Single {
        item: &'a ResponseItem,
    },
    Grouped {
        commit_call_id: &'a str,
        tool_call_ids: &'a [String],
        output_items: &'a [ResponseItem],
        force_ordinary: bool,
    },
    GroupedAlreadyRecorded {
        commit_call_id: &'a str,
        tool_call_ids: &'a [String],
        output_items: &'a [ResponseItem],
        output_raw_ordinals: &'a [Option<u64>],
        output_context_indices: &'a [usize],
    },
}

pub(in crate::spine) struct ToolcallHookEvidence<'a> {
    pub(in crate::spine) completed_output: &'a CompletedToolCallOutputEvidence<'a>,
    pub(in crate::spine) output_raw_ordinals: &'a [Option<u64>],
    pub(in crate::spine) output_context_start: usize,
    pub(in crate::spine) raw_items: &'a [Option<ResponseItem>],
    pub(in crate::spine) current_turn_provider_input_tokens: Option<i64>,
    pub(in crate::spine) tool_resp_already_recorded: bool,
    pub(in crate::spine) recorded_inside_reduce: bool,
}

#[derive(Clone, Copy)]
pub(in crate::spine) struct CompletedToolCallOutputEvidence<'a> {
    pub(in crate::spine) inner: runtime::SpineCompletedToolCallOutputEvidence<'a>,
    pub(in crate::spine) already_recorded_anchor: Option<(&'a [Option<u64>], usize)>,
    already_recorded_response_context_indices: Option<&'a [usize]>,
}

impl<'a> ToolCallEvidence<'a> {
    pub(crate) fn single(item: &'a ResponseItem) -> Self {
        Self {
            kind: ToolCallEvidenceKind::Single { item },
        }
    }

    pub(crate) fn grouped(
        commit_call_id: &'a str,
        tool_call_ids: &'a [String],
        output_items: &'a [ResponseItem],
    ) -> Self {
        Self::grouped_with_policy(commit_call_id, tool_call_ids, output_items, false)
    }

    pub(crate) fn grouped_as_ordinary(
        commit_call_id: &'a str,
        tool_call_ids: &'a [String],
        output_items: &'a [ResponseItem],
    ) -> Self {
        Self::grouped_with_policy(commit_call_id, tool_call_ids, output_items, true)
    }

    fn grouped_with_policy(
        commit_call_id: &'a str,
        tool_call_ids: &'a [String],
        output_items: &'a [ResponseItem],
        force_ordinary: bool,
    ) -> Self {
        Self {
            kind: ToolCallEvidenceKind::Grouped {
                commit_call_id,
                tool_call_ids,
                output_items,
                force_ordinary,
            },
        }
    }

    pub(crate) fn grouped_already_recorded(
        commit_call_id: &'a str,
        tool_call_ids: &'a [String],
        output_items: &'a [ResponseItem],
        output_raw_ordinals: &'a [Option<u64>],
        output_context_indices: &'a [usize],
    ) -> Self {
        Self {
            kind: ToolCallEvidenceKind::GroupedAlreadyRecorded {
                commit_call_id,
                tool_call_ids,
                output_items,
                output_raw_ordinals,
                output_context_indices,
            },
        }
    }

    pub(in crate::spine) fn completed_output(
        &self,
    ) -> Result<Option<CompletedToolCallOutputEvidence<'a>>, SpineError> {
        let (output, already_recorded_anchor, already_recorded_response_context_indices) =
            match &self.kind {
                ToolCallEvidenceKind::Single { item } => (
                    runtime::SpineToolCallEvidence::single(item).completed_output()?,
                    None,
                    None,
                ),
                ToolCallEvidenceKind::Grouped {
                    commit_call_id,
                    tool_call_ids,
                    output_items,
                    force_ordinary,
                } => {
                    let output = grouped_runtime_toolcall_evidence(
                        commit_call_id,
                        tool_call_ids,
                        output_items,
                        *force_ordinary,
                    )
                    .completed_output()?;
                    (output, None, None)
                }
                ToolCallEvidenceKind::GroupedAlreadyRecorded {
                    commit_call_id,
                    tool_call_ids,
                    output_items,
                    output_raw_ordinals,
                    output_context_indices,
                } => {
                    let output = runtime::SpineToolCallEvidence::grouped(
                        commit_call_id,
                        tool_call_ids,
                        output_items,
                    )
                    .completed_output()?;
                    let already_recorded_anchor = output_context_indices
                        .first()
                        .copied()
                        .map(|context_start| (*output_raw_ordinals, context_start));
                    (
                        output,
                        already_recorded_anchor,
                        Some(*output_context_indices),
                    )
                }
            };
        Ok(output.map(|output| CompletedToolCallOutputEvidence {
            inner: output,
            already_recorded_anchor,
            already_recorded_response_context_indices,
        }))
    }
}

fn grouped_runtime_toolcall_evidence<'a>(
    commit_call_id: &'a str,
    tool_call_ids: &'a [String],
    output_items: &'a [ResponseItem],
    force_ordinary: bool,
) -> runtime::SpineToolCallEvidence<'a> {
    if force_ordinary {
        runtime::SpineToolCallEvidence::grouped_as_ordinary(
            commit_call_id,
            tool_call_ids,
            output_items,
        )
    } else {
        runtime::SpineToolCallEvidence::grouped(commit_call_id, tool_call_ids, output_items)
    }
}

impl<'a> ToolcallHookEvidence<'a> {
    pub(in crate::spine) fn new(
        completed_output: &'a CompletedToolCallOutputEvidence<'a>,
        output_raw_ordinals: &'a [Option<u64>],
        output_context_start: usize,
        raw_items: &'a [Option<ResponseItem>],
        current_turn_provider_input_tokens: Option<i64>,
        tool_resp_already_recorded: bool,
        recorded_inside_reduce: bool,
    ) -> Self {
        Self {
            completed_output,
            output_raw_ordinals,
            output_context_start,
            raw_items,
            current_turn_provider_input_tokens,
            tool_resp_already_recorded,
            recorded_inside_reduce,
        }
    }

    pub(super) fn into_runtime(self) -> runtime::SpineToolcallHookEvidence<'a> {
        runtime::SpineToolcallHookEvidence {
            completed_output: &self.completed_output.inner,
            output_raw_ordinals: self.output_raw_ordinals,
            output_context_start: self.output_context_start,
            output_context_indices: self
                .completed_output
                .already_recorded_response_context_indices,
            raw_items: self.raw_items,
            current_turn_provider_input_tokens: self.current_turn_provider_input_tokens,
            tool_resp_already_recorded: self.tool_resp_already_recorded,
            recorded_inside_reduce: self.recorded_inside_reduce,
        }
    }
}

impl<'a> CompletedToolCallOutputEvidence<'a> {
    pub(in crate::spine) fn call_id(&self) -> &'a str {
        self.inner.call_id()
    }

    pub(in crate::spine) fn commit_output_item(&self) -> &'a ResponseItem {
        self.inner.commit_output_item()
    }

    pub(in crate::spine) fn runtime_output(
        &self,
    ) -> &runtime::SpineCompletedToolCallOutputEvidence<'a> {
        &self.inner
    }

    pub(in crate::spine) fn single_output_requiring_optional_prerecord(
        &self,
    ) -> Option<(&'a str, &'a ResponseItem)> {
        self.inner.single_output_requiring_optional_prerecord()
    }

    pub(in crate::spine) fn source_evidence_already_recorded_anchor(
        &self,
    ) -> Option<(&'a [Option<u64>], usize)> {
        self.already_recorded_anchor
    }

    pub(in crate::spine) fn output_group_to_record_before_commit(
        &self,
    ) -> Option<&'a [ResponseItem]> {
        self.inner.output_group_to_record_before_commit()
    }
}
