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
    pub(crate) fn raw_ordinals(&self) -> &[Option<u64>] {
        &self.raw_ordinals
    }

    pub(crate) fn into_raw_ordinals(self) -> Vec<Option<u64>> {
        self.raw_ordinals
    }
}
