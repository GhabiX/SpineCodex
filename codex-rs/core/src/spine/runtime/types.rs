use codex_protocol::models::ResponseItem;

use crate::spine::model::TrimBodyUpdate;
use std::ops::Range;

use crate::spine::model::NodeId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpinePendingCloseAction {
    Close,
    Next,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SpinePendingCommit {
    Open,
    Close {
        action: SpinePendingCloseAction,
        node: NodeId,
        suffix_start: usize,
        memory: String,
        next_summary: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpineCloseMemoryAssembly {
    pub(crate) body: String,
    pub(crate) source_context_range: Range<usize>,
    pub(crate) source_raw_range: Range<u64>,
    pub(crate) memory_output_tokens: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SpinePreparedCloseMemory {
    assembly: SpineCloseMemoryAssembly,
    expected_history: Vec<ResponseItem>,
}

impl SpinePreparedCloseMemory {
    pub(crate) fn new(
        assembly: SpineCloseMemoryAssembly,
        expected_history: Vec<ResponseItem>,
    ) -> Self {
        Self {
            assembly,
            expected_history,
        }
    }

    pub(crate) fn expected_history(&self) -> &[ResponseItem] {
        &self.expected_history
    }

    pub(crate) fn into_assembly(self) -> SpineCloseMemoryAssembly {
        self.assembly
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SpineCompactSourcePlan {
    pub(crate) node_id: NodeId,
    pub(crate) source_context_range: Range<usize>,
    pub(crate) source_raw_range: Range<u64>,
    pub(crate) entries: Vec<SpineCompactSourcePlanEntry>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SpineCompactSourcePlanEntry {
    pub(crate) context_index: usize,
    pub(crate) source_ordinal: usize,
    pub(crate) source_hash: String,
    pub(crate) kind: SpineCompactSourceEntryKind,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SpineCompactSourceEntryKind {
    RawResponseItem {
        item: ResponseItem,
        raw_ordinal: u64,
        from_user: bool,
        user_anchor: Option<u64>,
    },
    ChildMemory {
        node_id: NodeId,
        compact_id: String,
        source_raw_range: Range<u64>,
        rendered_context_item_count: Option<usize>,
        body: String,
        body_hash: String,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SpineTokenBaselines {
    pub(crate) provider_input_tokens: Option<i64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SpineRootCompactTokenMetadata {
    pub(crate) close_input_tokens: Option<i64>,
    pub(crate) close_context_tokens: Option<i64>,
    pub(crate) next_open_input_tokens: Option<i64>,
    pub(crate) next_open_context_tokens: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpineOpenNodeContextProjection {
    pub(crate) node_id: NodeId,
    pub(crate) provider_input_tokens: Option<i64>,
    pub(crate) baseline_source: Option<codex_protocol::spine_tree::SpineNodeContextBaselineSource>,
    pub(crate) problem: Option<codex_protocol::spine_tree::SpineNodeContextProblem>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpineCurrentTrimTarget {
    pub(crate) trim_id: String,
    pub(crate) original_visible_size: i64,
    pub(crate) visible_body: String,
}

#[derive(Clone, Debug)]
pub(crate) struct SpineRootCompactResult {
    pub(crate) variable_context: Vec<ResponseItem>,
    pub(crate) raw_boundary: u64,
    pub(crate) token_seq_after: u64,
}

impl SpineRootCompactResult {
    pub(crate) fn variable_context(&self) -> &[ResponseItem] {
        &self.variable_context
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SpineTrimOutcome {
    Cleared { trim_id: String },
    AlreadyCleared { trim_id: String },
    Sliced { trim_id: String },
    Miss { trim_id: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpineTrimUpdateOutcome {
    outcome: SpineTrimOutcome,
    body_updates: Vec<TrimBodyUpdate>,
}

impl SpineTrimUpdateOutcome {
    pub(crate) fn without_updates(outcome: SpineTrimOutcome) -> Self {
        Self {
            outcome,
            body_updates: Vec::new(),
        }
    }

    pub(crate) fn with_update(outcome: SpineTrimOutcome, update: TrimBodyUpdate) -> Self {
        Self {
            outcome,
            body_updates: vec![update],
        }
    }

    pub(crate) fn into_parts(self) -> (SpineTrimOutcome, Vec<TrimBodyUpdate>) {
        (self.outcome, self.body_updates)
    }
}

impl SpineTrimOutcome {
    pub(crate) fn model_response_message(&self) -> String {
        match self {
            Self::Cleared { trim_id } => format!("Trimmed tool response {trim_id}."),
            Self::AlreadyCleared { trim_id } => {
                format!("Tool response {trim_id} was already cleared.")
            }
            Self::Sliced { trim_id } => format!("Sliced tool response {trim_id}."),
            Self::Miss { trim_id } => {
                format!(
                    "Could not find trim id {trim_id} in the latest returned tool-result batch. Do not retry this TRIM_ID."
                )
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct LiveRootCompact {
    pub(crate) raw_boundary: u64,
    pub(crate) token_seq: u64,
}
