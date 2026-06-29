use codex_protocol::models::ResponseItem;

use super::super::runtime::SpineError;
use super::super::runtime::SpineSessionState;

pub(crate) struct SingleToolcallOutputRecordingPlan {
    pub(super) raw_len: u64,
    pub(super) prerecord_output_before_reduce: bool,
}

pub(crate) struct GroupedToolcallOutputRecordingPlan {
    pub(super) raw_ordinals: Vec<Option<u64>>,
}

pub(super) fn prepare_single_output_recording(
    state: &SpineSessionState,
    call_id: &str,
    raw_items: &[Option<ResponseItem>],
) -> Result<Option<SingleToolcallOutputRecordingPlan>, SpineError> {
    state
        .prepare_single_toolcall_output_recording(call_id, raw_items)
        .map(|plan| {
            plan.map(|plan| SingleToolcallOutputRecordingPlan {
                raw_len: plan.raw_len,
                prerecord_output_before_reduce: plan.prerecord_output_before_reduce,
            })
        })
}

pub(super) fn prepare_grouped_output_recording(
    state: &SpineSessionState,
    output_items: &[ResponseItem],
) -> Result<GroupedToolcallOutputRecordingPlan, SpineError> {
    state
        .prepare_grouped_toolcall_output_recording(output_items)
        .map(|plan| GroupedToolcallOutputRecordingPlan {
            raw_ordinals: plan.raw_ordinals,
        })
}
