use codex_protocol::models::ResponseItem;

use super::super::runtime;
use super::super::runtime::SpineError;
use super::super::runtime::SpineSessionState;
use crate::spine::model::TrimBodyUpdate;

pub(crate) type TrimOutcome = runtime::SpineTrimOutcome;
pub(crate) type TrimUpdateOutcome = runtime::SpineTrimUpdateOutcome;

pub(crate) enum TrimRequest<'a> {
    Snip,
    SliceHead {
        head: usize,
    },
    SliceTail {
        tail: usize,
    },
    SliceAnchor {
        anchor: &'a str,
        preceding: usize,
        following: usize,
    },
}

impl TrimRequest<'_> {
    pub(crate) fn needs_raw_items(&self) -> bool {
        !matches!(self, Self::Snip)
    }
}

pub(crate) struct TrimRuntime;

impl TrimRuntime {
    pub(crate) fn projection_needs_rollout_raw_items(
        state: &SpineSessionState,
    ) -> Result<Option<bool>, SpineError> {
        state.trim_projection_needs_rollout_raw_items()
    }

    pub(crate) fn current_trim_body_updates(
        state: &SpineSessionState,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<Option<Vec<TrimBodyUpdate>>, SpineError> {
        state.current_trim_body_updates(raw_items)
    }

    pub(crate) fn observe_recorded_tool_output_group_as_completed_toolcall(
        state: &mut SpineSessionState,
        tool_responses: &[(String, u64, usize)],
        raw_items: &[Option<ResponseItem>],
    ) -> Result<Vec<TrimBodyUpdate>, SpineError> {
        state.observe_recorded_tool_output_group_as_completed_toolcall(tool_responses, raw_items)
    }

    pub(crate) fn apply_tool_response_request(
        state: &mut SpineSessionState,
        trim_id: &str,
        request: TrimRequest<'_>,
        raw_items: Option<&[Option<ResponseItem>]>,
    ) -> Result<TrimUpdateOutcome, SpineError> {
        match request {
            TrimRequest::Snip => state.trim_tool_response_with_updates(trim_id),
            TrimRequest::SliceHead { head } => {
                let raw_items = raw_items.ok_or_else(|| {
                    SpineError::InvalidEvent(
                        "spine trim slice_head requires raw rollout items".to_string(),
                    )
                })?;
                state.slice_tool_response_head_with_updates(trim_id, head, raw_items)
            }
            TrimRequest::SliceTail { tail } => {
                let raw_items = raw_items.ok_or_else(|| {
                    SpineError::InvalidEvent(
                        "spine trim slice_tail requires raw rollout items".to_string(),
                    )
                })?;
                state.slice_tool_response_tail_with_updates(trim_id, tail, raw_items)
            }
            TrimRequest::SliceAnchor {
                anchor,
                preceding,
                following,
            } => {
                let raw_items = raw_items.ok_or_else(|| {
                    SpineError::InvalidEvent(
                        "spine trim slice_anchor requires raw rollout items".to_string(),
                    )
                })?;
                state.slice_tool_response_anchor_with_updates(
                    trim_id, anchor, preceding, following, raw_items,
                )
            }
        }
    }
}
