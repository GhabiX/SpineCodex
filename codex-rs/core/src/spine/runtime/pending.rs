use crate::spine::model::ToolCallSegmentKind;

#[derive(Clone, Debug)]
pub(super) struct OpenRequestAnchor {
    pub(super) raw_ordinal: u64,
    pub(super) context_index: u64,
}

#[derive(Clone, Debug)]
pub(super) struct PendingMemoryContextAccounting {
    pub(super) compact_id: String,
    pub(super) replacement_prefix_baseline_tokens: i64,
    pub(super) close_input_tokens: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ToolRequestAnchor {
    pub(crate) raw_ordinal: u64,
    pub(crate) context_index: usize,
}

#[derive(Clone, Debug)]
pub(super) enum PendingTransition {
    Open {
        call_id: String,
        summary: String,
        boundary: u64,
        index: u64,
    },
    Close {
        call_id: String,
        memory: String,
    },
    NextSugar {
        call_id: String,
        summary: String,
        memory: String,
    },
}

impl PendingTransition {
    pub(super) fn call_id(&self) -> &str {
        match self {
            Self::Open { call_id, .. }
            | Self::Close { call_id, .. }
            | Self::NextSugar { call_id, .. } => call_id,
        }
    }
}

#[derive(Clone, Debug)]
pub(super) enum SpineControlToolReceipt {
    Open { summary: String },
    Close { memory: String },
    Next { summary: String, memory: String },
}

impl SpineControlToolReceipt {
    pub(super) fn is_close_like(&self) -> bool {
        matches!(self, Self::Close { .. } | Self::Next { .. })
    }
}

#[derive(Clone, Debug)]
pub(super) struct PendingMsg {
    pub(super) raw_ordinal: u64,
    pub(super) context_index: u64,
    pub(super) from_user: bool,
    pub(super) user_anchor: Option<u64>,
}

#[derive(Clone, Debug)]
pub(super) struct PendingToolRequest {
    pub(super) raw_ordinal: u64,
    pub(super) context_index: u64,
}

#[cfg(test)]
#[derive(Clone, Debug)]
pub(super) struct PendingToolResponse {
    pub(super) raw_ordinal: u64,
    pub(super) context_index: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct CompletedToolCall {
    pub(crate) call_id: String,
    pub(crate) request_call_ids: Vec<String>,
    pub(crate) segments: Vec<CompletedToolCallSegment>,
}

#[derive(Clone, Debug)]
pub(crate) struct CompletedToolCallSegment {
    pub(crate) kind: ToolCallSegmentKind,
    pub(crate) raw_ordinal: u64,
    pub(crate) context_index: usize,
}
