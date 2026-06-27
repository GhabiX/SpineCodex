use codex_protocol::models::ResponseItem;

use super::super::runtime;
use super::super::runtime::SpineError;
use super::super::runtime::SpineSessionState;

pub(crate) type TrimOutcome = runtime::SpineTrimOutcome;

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

    pub(crate) fn materialize_projection_from_raw_items(
        state: &SpineSessionState,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<Option<Vec<ResponseItem>>, SpineError> {
        state.materialize_trim_projection_from_raw_items(raw_items)
    }

    pub(crate) fn project_from_history(
        state: &SpineSessionState,
        history_items: &[ResponseItem],
    ) -> Result<Option<Vec<ResponseItem>>, SpineError> {
        state.project_trim_projection_from_history(history_items)
    }

    pub(crate) fn trim_tool_response(
        state: &mut SpineSessionState,
        trim_id: &str,
    ) -> Result<TrimOutcome, SpineError> {
        state.trim_tool_response(trim_id)
    }

    pub(crate) fn slice_tool_response_head(
        state: &mut SpineSessionState,
        trim_id: &str,
        head: usize,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<TrimOutcome, SpineError> {
        state.slice_tool_response_head(trim_id, head, raw_items)
    }

    pub(crate) fn slice_tool_response_tail(
        state: &mut SpineSessionState,
        trim_id: &str,
        tail: usize,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<TrimOutcome, SpineError> {
        state.slice_tool_response_tail(trim_id, tail, raw_items)
    }

    pub(crate) fn slice_tool_response_anchor(
        state: &mut SpineSessionState,
        trim_id: &str,
        anchor: &str,
        preceding: usize,
        following: usize,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<TrimOutcome, SpineError> {
        state.slice_tool_response_anchor(trim_id, anchor, preceding, following, raw_items)
    }

    pub(crate) fn apply_tool_response_request(
        state: &mut SpineSessionState,
        trim_id: &str,
        request: TrimRequest<'_>,
        raw_items: Option<&[Option<ResponseItem>]>,
    ) -> Result<TrimOutcome, SpineError> {
        match request {
            TrimRequest::Snip => Self::trim_tool_response(state, trim_id),
            TrimRequest::SliceHead { head } => {
                let raw_items = raw_items.ok_or_else(|| {
                    SpineError::InvalidEvent(
                        "spine trim slice_head requires raw rollout items".to_string(),
                    )
                })?;
                Self::slice_tool_response_head(state, trim_id, head, raw_items)
            }
            TrimRequest::SliceTail { tail } => {
                let raw_items = raw_items.ok_or_else(|| {
                    SpineError::InvalidEvent(
                        "spine trim slice_tail requires raw rollout items".to_string(),
                    )
                })?;
                Self::slice_tool_response_tail(state, trim_id, tail, raw_items)
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
                Self::slice_tool_response_anchor(
                    state, trim_id, anchor, preceding, following, raw_items,
                )
            }
        }
    }
}
