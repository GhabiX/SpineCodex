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
    #[cfg(test)]
    Runtime(runtime::SpineToolCallEvidence<'a>),
}

pub(crate) struct ToolcallHookEvidence<'a> {
    pub(crate) completed_output: &'a CompletedToolCallOutputEvidence<'a>,
    pub(crate) output_raw_ordinals: &'a [Option<u64>],
    pub(crate) output_context_start: usize,
    pub(crate) raw_items: &'a [Option<ResponseItem>],
    pub(crate) current_turn_provider_input_tokens: Option<i64>,
    pub(crate) tool_resp_already_recorded: bool,
    pub(crate) recorded_inside_reduce: bool,
}

#[derive(Clone, Copy)]
pub(crate) struct CompletedToolCallOutputEvidence<'a> {
    pub(in crate::spine) inner: runtime::SpineCompletedToolCallOutputEvidence<'a>,
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

    pub(crate) fn completed_output(
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
            #[cfg(test)]
            ToolCallEvidenceKind::Runtime(evidence) => evidence.completed_output(),
        }?;
        Ok(output.map(CompletedToolCallOutputEvidence::from_runtime))
    }
}

impl<'a> CompletedToolCallOutputEvidence<'a> {
    fn from_runtime(inner: runtime::SpineCompletedToolCallOutputEvidence<'a>) -> Self {
        Self { inner }
    }

    pub(crate) fn call_id(&self) -> &'a str {
        self.inner.call_id()
    }

    pub(crate) fn commit_output_item(&self) -> &'a ResponseItem {
        self.inner.commit_output_item()
    }

    pub(crate) fn single_output_requiring_optional_prerecord(
        &self,
    ) -> Option<(&'a str, &'a ResponseItem)> {
        self.inner.single_output_requiring_optional_prerecord()
    }

    pub(crate) fn output_group_to_record_before_commit(&self) -> Option<&'a [ResponseItem]> {
        self.inner.output_group_to_record_before_commit()
    }

    pub(in crate::spine) fn runtime_output(
        &self,
    ) -> &runtime::SpineCompletedToolCallOutputEvidence<'a> {
        &self.inner
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
