use super::raw_mask::RawMask;
use super::token::ContextBaselineSource;
use super::token::NodeId;
use super::token::ToolCallEventSegment;
use crate::spine::SpineError;
use serde::Deserialize;
use serde::Serialize;

/// Durable sidecar event ledger, replayed into SpineToken for ParseStack.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(in crate::spine) enum SpineLedgerEvent {
    Init {
        raw_start: u64,
    },
    Msg {
        raw_ordinal: u64,
        context_index: u64,
        from_user: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        user_anchor: Option<u64>,
    },
    ToolCall {
        segments: Vec<ToolCallEventSegment>,
    },
    Open {
        child: NodeId,
        boundary: u64,
        index: u64,
        summary: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        open_input_tokens: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        open_context_tokens: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        open_context_source: Option<ContextBaselineSource>,
    },
    Close {
        node: NodeId,
        boundary: u64,
        summary: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        close_input_tokens: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        close_context_tokens: Option<i64>,
    },
    RootCompact {
        node: NodeId,
        boundary: u64,
        mem: String,
        next_open_index: u64,
        raw_live_hash: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next_open_input_tokens: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next_open_context_tokens: Option<i64>,
    },
    OpenContextBaseline {
        node: NodeId,
        raw_boundary: u64,
        raw_live_hash: String,
        open_input_tokens: i64,
        open_context_tokens: i64,
        open_context_source: ContextBaselineSource,
    },
}

/// Append-only Spine ledger event with a monotonic sidecar sequence number.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(in crate::spine) struct LoggedSpineLedgerEvent {
    pub(in crate::spine) seq: u64,
    #[serde(flatten)]
    pub(in crate::spine) event: SpineLedgerEvent,
}

impl LoggedSpineLedgerEvent {
    pub(in crate::spine) fn allowed_by(&self, raw_mask: RawMask<'_>) -> Result<bool, SpineError> {
        self.event.allowed_by(raw_mask)
    }
}

pub(in crate::spine) fn next_seq_from(
    seqs: impl Iterator<Item = u64>,
    overflow_message: &str,
) -> Result<u64, SpineError> {
    seqs.max().map_or(Ok(0), |seq| {
        seq.checked_add(1)
            .ok_or_else(|| SpineError::InvalidEvent(overflow_message.to_string()))
    })
}

impl SpineLedgerEvent {
    pub(in crate::spine) fn is_root_epoch_open(&self) -> bool {
        matches!(
            self,
            SpineLedgerEvent::Open { child, summary, .. }
                if summary == "root" && child.is_root_epoch_child()
        )
    }

    pub(in crate::spine) fn allowed_by(&self, raw_mask: RawMask<'_>) -> Result<bool, SpineError> {
        match self {
            SpineLedgerEvent::Init { .. } => Ok(true),
            SpineLedgerEvent::Msg { raw_ordinal, .. } => raw_mask.raw_index_live(*raw_ordinal),
            SpineLedgerEvent::ToolCall { segments } => {
                segments.iter().try_fold(true, |live, segment| {
                    Ok(live && raw_mask.raw_index_live(segment.raw_ordinal)?)
                })
            }
            SpineLedgerEvent::Open { .. } if self.is_root_epoch_open() => Ok(true),
            SpineLedgerEvent::Open { boundary, .. } => raw_mask.raw_index_live(*boundary),
            SpineLedgerEvent::Close { boundary, .. } => raw_mask.boundary_live(*boundary),
            SpineLedgerEvent::RootCompact {
                boundary,
                raw_live_hash,
                ..
            } => raw_mask.prefix_hash_matches(*boundary, raw_live_hash),
            SpineLedgerEvent::OpenContextBaseline {
                raw_boundary,
                raw_live_hash,
                ..
            } => raw_mask.prefix_hash_matches(*raw_boundary, raw_live_hash),
        }
    }
}
