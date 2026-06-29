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
    #[cfg(test)]
    Runtime(runtime::SpineToolCallEvidence<'a>),
}

pub(in crate::spine) struct ToolcallHookEvidence<'a> {
    completed_output: &'a CompletedToolCallOutputEvidence<'a>,
    output_raw_ordinals: &'a [Option<u64>],
    output_context_start: usize,
    raw_items: &'a [Option<ResponseItem>],
    current_turn_provider_input_tokens: Option<i64>,
    tool_resp_already_recorded: bool,
    recorded_inside_reduce: bool,
}

#[derive(Clone, Copy)]
pub(in crate::spine) struct CompletedToolCallOutputEvidence<'a> {
    inner: runtime::SpineCompletedToolCallOutputEvidence<'a>,
    already_recorded_anchor: Option<(&'a [Option<u64>], usize)>,
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
        Self {
            kind: ToolCallEvidenceKind::Grouped {
                commit_call_id,
                tool_call_ids,
                output_items,
                force_ordinary: false,
            },
        }
    }

    pub(crate) fn grouped_as_ordinary(
        commit_call_id: &'a str,
        tool_call_ids: &'a [String],
        output_items: &'a [ResponseItem],
    ) -> Self {
        Self {
            kind: ToolCallEvidenceKind::Grouped {
                commit_call_id,
                tool_call_ids,
                output_items,
                force_ordinary: true,
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
        let output = match &self.kind {
            ToolCallEvidenceKind::Single { item } => {
                runtime::SpineToolCallEvidence::single(item).completed_output()
            }
            ToolCallEvidenceKind::Grouped {
                commit_call_id,
                tool_call_ids,
                output_items,
                force_ordinary,
            } => {
                if *force_ordinary {
                    runtime::SpineToolCallEvidence::grouped_as_ordinary(
                        commit_call_id,
                        tool_call_ids,
                        output_items,
                    )
                    .completed_output()
                } else {
                    runtime::SpineToolCallEvidence::grouped(
                        commit_call_id,
                        tool_call_ids,
                        output_items,
                    )
                    .completed_output()
                }
            }
            ToolCallEvidenceKind::GroupedAlreadyRecorded {
                commit_call_id,
                tool_call_ids,
                output_items,
                ..
            } => {
                runtime::SpineToolCallEvidence::grouped(commit_call_id, tool_call_ids, output_items)
                    .completed_output()
            }
            #[cfg(test)]
            ToolCallEvidenceKind::Runtime(evidence) => evidence.completed_output(),
        }?;
        let already_recorded_anchor = self.already_recorded_output_anchor();
        let already_recorded_context_indices = self.already_recorded_response_context_indices();
        Ok(output.map(|output| {
            CompletedToolCallOutputEvidence::from_runtime(
                output,
                already_recorded_anchor,
                already_recorded_context_indices,
            )
        }))
    }

    fn already_recorded_output_anchor(&self) -> Option<(&'a [Option<u64>], usize)> {
        match &self.kind {
            ToolCallEvidenceKind::GroupedAlreadyRecorded {
                output_raw_ordinals,
                output_context_indices,
                ..
            } => output_context_indices
                .first()
                .copied()
                .map(|context_start| (*output_raw_ordinals, context_start)),
            ToolCallEvidenceKind::Single { .. } | ToolCallEvidenceKind::Grouped { .. } => None,
            #[cfg(test)]
            ToolCallEvidenceKind::Runtime(_) => None,
        }
    }

    fn already_recorded_response_context_indices(&self) -> Option<&'a [usize]> {
        match &self.kind {
            ToolCallEvidenceKind::GroupedAlreadyRecorded {
                output_context_indices,
                ..
            } => Some(*output_context_indices),
            ToolCallEvidenceKind::Single { .. } | ToolCallEvidenceKind::Grouped { .. } => None,
            #[cfg(test)]
            ToolCallEvidenceKind::Runtime(_) => None,
        }
    }
}

impl<'a> CompletedToolCallOutputEvidence<'a> {
    fn from_runtime(
        inner: runtime::SpineCompletedToolCallOutputEvidence<'a>,
        already_recorded_anchor: Option<(&'a [Option<u64>], usize)>,
        already_recorded_response_context_indices: Option<&'a [usize]>,
    ) -> Self {
        Self {
            inner,
            already_recorded_anchor,
            already_recorded_response_context_indices,
        }
    }

    pub(in crate::spine) fn call_id(&self) -> &'a str {
        self.inner.call_id()
    }

    pub(in crate::spine) fn commit_output_item(&self) -> &'a ResponseItem {
        self.inner.commit_output_item()
    }

    pub(in crate::spine) fn single_output_requiring_optional_prerecord(
        &self,
    ) -> Option<(&'a str, &'a ResponseItem)> {
        self.inner.single_output_requiring_optional_prerecord()
    }

    pub(in crate::spine) fn output_group_to_record_before_commit(
        &self,
    ) -> Option<&'a [ResponseItem]> {
        self.inner.output_group_to_record_before_commit()
    }

    pub(in crate::spine) fn source_evidence_already_recorded_anchor(
        &self,
    ) -> Option<(&'a [Option<u64>], usize)> {
        self.already_recorded_anchor
    }

    pub(in crate::spine) fn source_evidence_already_recorded_response_context_indices(
        &self,
    ) -> Option<&'a [usize]> {
        self.already_recorded_response_context_indices
    }

    pub(in crate::spine) fn runtime_output(
        &self,
    ) -> &runtime::SpineCompletedToolCallOutputEvidence<'a> {
        &self.inner
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
            completed_output: self.completed_output.runtime_output(),
            output_raw_ordinals: self.output_raw_ordinals,
            output_context_start: self.output_context_start,
            output_context_indices: self
                .completed_output
                .source_evidence_already_recorded_response_context_indices(),
            raw_items: self.raw_items,
            current_turn_provider_input_tokens: self.current_turn_provider_input_tokens,
            tool_resp_already_recorded: self.tool_resp_already_recorded,
            recorded_inside_reduce: self.recorded_inside_reduce,
        }
    }
}

#[cfg(test)]
impl<'a> From<runtime::SpineToolCallEvidence<'a>> for ToolCallEvidence<'a> {
    fn from(evidence: runtime::SpineToolCallEvidence<'a>) -> Self {
        Self {
            kind: ToolCallEvidenceKind::Runtime(evidence),
        }
    }
}
