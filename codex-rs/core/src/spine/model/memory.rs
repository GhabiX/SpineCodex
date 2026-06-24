use super::raw_mask::RawMask;
use super::token::ContextBaselineSource;
use super::token::NodeId;
use crate::spine::SpineError;
use serde::Deserialize;
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::spine) enum MemKind {
    Suffix,
    RootEpoch,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::spine) struct MemRecord {
    pub(in crate::spine) compact_id: String,
    pub(in crate::spine) kind: MemKind,
    pub(in crate::spine) node: NodeId,
    pub(in crate::spine) raw_start: u64,
    pub(in crate::spine) raw_end: u64,
    pub(in crate::spine) context_start: usize,
    pub(in crate::spine) context_end: usize,
    #[serde(default)]
    pub(in crate::spine) raw_live_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::spine) open_input_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::spine) close_input_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::spine) open_context_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::spine) close_context_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::spine) closed_source_suffix_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::spine) closed_memory_context_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::spine) open_context_source: Option<ContextBaselineSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::spine) memory_output_tokens: Option<i64>,
    pub(in crate::spine) body_path: String,
    pub(in crate::spine) body_hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::spine) struct MemoryContextAccountingRecord {
    pub(in crate::spine) compact_id: String,
    pub(in crate::spine) closed_memory_context_tokens: i64,
    pub(in crate::spine) provider_input_tokens: i64,
    pub(in crate::spine) replacement_prefix_baseline_tokens: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::spine) enum MemoryContextAccountingSkipReason {
    MissingProviderUsage,
    InvalidProviderUsage,
    NegativeMemoryDelta,
    SupersededByNewPending,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(in crate::spine) enum MemoryContextAccountingWitnessRecord {
    Pending {
        compact_id: String,
        replacement_prefix_baseline_tokens: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        close_input_tokens: Option<i64>,
    },
    Consumed {
        compact_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_input_tokens: Option<i64>,
        reason: MemoryContextAccountingSkipReason,
    },
}

impl MemoryContextAccountingWitnessRecord {
    pub(in crate::spine) fn compact_id(&self) -> &str {
        match self {
            Self::Pending { compact_id, .. } | Self::Consumed { compact_id, .. } => compact_id,
        }
    }
}

impl MemRecord {
    pub(in crate::spine) fn allowed_by(&self, raw_mask: RawMask<'_>) -> Result<bool, SpineError> {
        match self.kind {
            MemKind::Suffix => raw_mask.span_live(self.raw_start, self.raw_end),
            MemKind::RootEpoch => {
                raw_mask.optional_prefix_hash_matches(self.raw_end, self.raw_live_hash.as_deref())
            }
        }
    }
}
