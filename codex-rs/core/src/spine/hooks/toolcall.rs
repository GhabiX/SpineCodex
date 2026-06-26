use codex_protocol::models::ResponseItem;

use super::super::runtime;
use super::super::runtime::SpineError;
use super::super::runtime::SpineSessionState;

pub(crate) struct ToolcallOutputRecordingRequest<'a> {
    inner: runtime::SpineToolcallOutputRecordingRequest<'a>,
}

pub(crate) enum ToolcallOutputRecordingPlan {
    Single(Option<SingleToolcallOutputRecordingPlan>),
    Grouped(GroupedToolcallOutputRecordingPlan),
}

pub(crate) struct SingleToolcallOutputRecordingPlan {
    raw_len: u64,
    prerecord_output_before_reduce: bool,
}

pub(crate) struct GroupedToolcallOutputRecordingPlan {
    raw_ordinals: Vec<Option<u64>>,
}

pub(crate) struct ToolCallEvidence<'a> {
    inner: runtime::SpineToolCallEvidence<'a>,
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

pub(crate) struct CompletedToolCallOutputEvidence<'a> {
    pub(super) inner: runtime::SpineCompletedToolCallOutputEvidence<'a>,
}

impl<'a> ToolCallEvidence<'a> {
    pub(crate) fn single(item: &'a ResponseItem) -> Self {
        Self {
            inner: runtime::SpineToolCallEvidence::single(item),
        }
    }

    pub(crate) fn grouped(
        commit_call_id: &'a str,
        tool_call_ids: &'a [String],
        output_items: &'a [ResponseItem],
    ) -> Self {
        Self {
            inner: runtime::SpineToolCallEvidence::grouped(
                commit_call_id,
                tool_call_ids,
                output_items,
            ),
        }
    }

    pub(crate) fn grouped_as_ordinary(
        commit_call_id: &'a str,
        tool_call_ids: &'a [String],
        output_items: &'a [ResponseItem],
    ) -> Self {
        Self {
            inner: runtime::SpineToolCallEvidence::grouped_as_ordinary(
                commit_call_id,
                tool_call_ids,
                output_items,
            ),
        }
    }

    pub(crate) fn completed_output(
        &self,
    ) -> Result<Option<CompletedToolCallOutputEvidence<'a>>, SpineError> {
        self.inner
            .completed_output()
            .map(|output| output.map(CompletedToolCallOutputEvidence::from_runtime))
    }
}

impl<'a> ToolcallOutputRecordingRequest<'a> {
    pub(crate) fn single(call_id: &'a str, raw_items: &'a [Option<ResponseItem>]) -> Self {
        Self {
            inner: runtime::SpineToolcallOutputRecordingRequest::Single { call_id, raw_items },
        }
    }

    pub(crate) fn grouped(output_items: &'a [ResponseItem]) -> Self {
        Self {
            inner: runtime::SpineToolcallOutputRecordingRequest::Grouped { output_items },
        }
    }

    fn into_runtime(self) -> runtime::SpineToolcallOutputRecordingRequest<'a> {
        self.inner
    }

    pub(crate) fn prepare(
        self,
        state: &SpineSessionState,
    ) -> Result<ToolcallOutputRecordingPlan, SpineError> {
        state
            .prepare_toolcall_output_recording(self.into_runtime())
            .map(ToolcallOutputRecordingPlan::from_runtime)
    }
}

impl ToolcallOutputRecordingPlan {
    fn from_runtime(inner: runtime::SpineToolcallOutputRecordingPlan) -> Self {
        match inner {
            runtime::SpineToolcallOutputRecordingPlan::Single(plan) => {
                Self::Single(plan.map(|plan| SingleToolcallOutputRecordingPlan {
                    raw_len: plan.raw_len(),
                    prerecord_output_before_reduce: plan.prerecord_output_before_reduce(),
                }))
            }
            runtime::SpineToolcallOutputRecordingPlan::Grouped(plan) => {
                Self::Grouped(GroupedToolcallOutputRecordingPlan {
                    raw_ordinals: plan.into_raw_ordinals(),
                })
            }
        }
    }
}

impl SingleToolcallOutputRecordingPlan {
    pub(crate) fn raw_len(&self) -> u64 {
        self.raw_len
    }

    pub(crate) fn prerecord_output_before_reduce(&self) -> bool {
        self.prerecord_output_before_reduce
    }
}

impl GroupedToolcallOutputRecordingPlan {
    pub(crate) fn into_raw_ordinals(self) -> Vec<Option<u64>> {
        self.raw_ordinals
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
}

#[cfg(test)]
impl<'a> From<runtime::SpineToolCallEvidence<'a>> for ToolCallEvidence<'a> {
    fn from(evidence: runtime::SpineToolCallEvidence<'a>) -> Self {
        Self { inner: evidence }
    }
}
