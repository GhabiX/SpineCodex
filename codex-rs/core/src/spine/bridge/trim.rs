use codex_protocol::models::ResponseItem;

use super::super::runtime::SpineError;
use super::super::runtime::SpineSessionState;
use super::super::runtime::SpineTrimUpdateOutcome;

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

    pub(crate) fn apply_to_state(
        self,
        state: &mut SpineSessionState,
        trim_id: &str,
        raw_items: Option<&[Option<ResponseItem>]>,
    ) -> Result<SpineTrimUpdateOutcome, SpineError> {
        match self {
            Self::Snip => state.trim_tool_response_with_updates(trim_id),
            Self::SliceHead { head } => {
                let raw_items = raw_items.ok_or_else(|| {
                    SpineError::InvalidEvent(
                        "spine trim slice_head requires raw rollout items".to_string(),
                    )
                })?;
                state.slice_tool_response_head_with_updates(trim_id, head, raw_items)
            }
            Self::SliceTail { tail } => {
                let raw_items = raw_items.ok_or_else(|| {
                    SpineError::InvalidEvent(
                        "spine trim slice_tail requires raw rollout items".to_string(),
                    )
                })?;
                state.slice_tool_response_tail_with_updates(trim_id, tail, raw_items)
            }
            Self::SliceAnchor {
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
