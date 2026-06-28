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
    pub(super) raw_len: u64,
    pub(super) prerecord_output_before_reduce: bool,
}

pub(crate) struct GroupedToolcallOutputRecordingPlan {
    pub(super) raw_ordinals: Vec<Option<u64>>,
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

    pub(crate) fn prepare(
        self,
        state: &SpineSessionState,
    ) -> Result<ToolcallOutputRecordingPlan, SpineError> {
        state
            .prepare_toolcall_output_recording(self.inner)
            .map(ToolcallOutputRecordingPlan::from_runtime)
    }
}

impl ToolcallOutputRecordingPlan {
    fn from_runtime(inner: runtime::SpineToolcallOutputRecordingPlan) -> Self {
        match inner {
            runtime::SpineToolcallOutputRecordingPlan::Single(plan) => {
                Self::Single(plan.map(|plan| SingleToolcallOutputRecordingPlan {
                    raw_len: plan.raw_len,
                    prerecord_output_before_reduce: plan.prerecord_output_before_reduce,
                }))
            }
            runtime::SpineToolcallOutputRecordingPlan::Grouped(plan) => {
                Self::Grouped(GroupedToolcallOutputRecordingPlan {
                    raw_ordinals: plan.raw_ordinals,
                })
            }
        }
    }
}
