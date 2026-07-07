use super::raw_mask::RawMask;
use crate::spine::SpineError;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;

pub(crate) const TOOL_RESPONSE_TRIM_THRESHOLD_BYTES: i64 = 10_000;
pub(crate) const TOOL_RESULT_CLEARED_MESSAGE: &str = "[Old tool result content cleared]";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TrimResponseKind {
    FunctionCallOutput,
    CustomToolCallOutput,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(in crate::spine) enum TrimEvent {
    ToolCallBoundary {
        toolcall_seq: u64,
        raw_boundary: u64,
        raw_live_hash: String,
    },
    Candidate {
        trim_id: String,
        toolcall_seq: u64,
        raw_ordinal: u64,
        call_id: String,
        response_kind: TrimResponseKind,
        original_visible_size: i64,
    },
    Cleared {
        trim_id: String,
        raw_boundary: u64,
        raw_live_hash: String,
    },
    Snipped {
        trim_id: String,
        raw_boundary: u64,
        raw_live_hash: String,
    },
    Sliced {
        trim_id: String,
        raw_boundary: u64,
        raw_live_hash: String,
        slice: TrimSliceSpec,
        visible_body: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(in crate::spine) struct LoggedTrimEvent {
    pub(in crate::spine) trim_seq: u64,
    #[serde(flatten)]
    pub(in crate::spine) event: TrimEvent,
}

impl LoggedTrimEvent {
    pub(in crate::spine) fn allowed_by(&self, raw_mask: RawMask<'_>) -> Result<bool, SpineError> {
        self.event.allowed_by(raw_mask)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::spine) struct TrimTarget {
    pub(in crate::spine) trim_id: String,
    pub(in crate::spine) toolcall_seq: u64,
    pub(in crate::spine) raw_ordinal: u64,
    pub(in crate::spine) call_id: String,
    pub(in crate::spine) response_kind: TrimResponseKind,
    pub(in crate::spine) original_visible_size: i64,
    pub(in crate::spine) state: TrimTargetState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::spine) enum TrimTargetState {
    Tagged,
    Snipped,
    Sliced { visible_body: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TrimBodyUpdate {
    pub(crate) trim_id: String,
    pub(crate) raw_ordinal: u64,
    pub(crate) call_id: String,
    pub(crate) response_kind: TrimResponseKind,
    pub(in crate::spine) state: TrimTargetState,
    pub(crate) visible_body: String,
}

impl TrimBodyUpdate {
    pub(in crate::spine) fn from_target(target: &TrimTarget, visible_body: String) -> Self {
        Self {
            trim_id: target.trim_id.clone(),
            raw_ordinal: target.raw_ordinal,
            call_id: target.call_id.clone(),
            response_kind: target.response_kind,
            state: target.state.clone(),
            visible_body,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(in crate::spine) enum TrimSliceSpec {
    Head {
        head: usize,
    },
    Tail {
        tail: usize,
    },
    Anchor {
        anchor: String,
        preceding: usize,
        following: usize,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(in crate::spine) struct TrimProjection {
    pub(in crate::spine) targets_by_id: BTreeMap<String, TrimTarget>,
    pub(in crate::spine) trim_id_by_raw_ordinal: BTreeMap<u64, String>,
}

impl TrimProjection {
    pub(in crate::spine) fn target_for_raw_ordinal(&self, raw_ordinal: u64) -> Option<&TrimTarget> {
        let trim_id = self.trim_id_by_raw_ordinal.get(&raw_ordinal)?;
        self.targets_by_id.get(trim_id)
    }

    pub(in crate::spine) fn insert_candidate(&mut self, target: TrimTarget) {
        self.trim_id_by_raw_ordinal
            .insert(target.raw_ordinal, target.trim_id.clone());
        self.targets_by_id.insert(target.trim_id.clone(), target);
    }

    pub(in crate::spine) fn contains_toolcall_raw_target(
        &self,
        toolcall_seq: u64,
        raw_ordinal: u64,
    ) -> bool {
        self.targets_by_id
            .values()
            .any(|target| target.toolcall_seq == toolcall_seq && target.raw_ordinal == raw_ordinal)
    }
}

impl TrimEvent {
    pub(in crate::spine) fn allowed_by(&self, raw_mask: RawMask<'_>) -> Result<bool, SpineError> {
        if let Some((raw_boundary, raw_live_hash)) = self.raw_live_prefix_proof() {
            return raw_mask.prefix_hash_matches(raw_boundary, raw_live_hash);
        }
        match self {
            TrimEvent::Candidate { raw_ordinal, .. } => raw_mask.raw_index_live(*raw_ordinal),
            TrimEvent::ToolCallBoundary { .. }
            | TrimEvent::Cleared { .. }
            | TrimEvent::Snipped { .. }
            | TrimEvent::Sliced { .. } => unreachable!("raw-live proof events returned above"),
        }
    }

    pub(in crate::spine) fn within_toolcall_boundary(&self, toolcall_seq_limit: u64) -> bool {
        match self {
            TrimEvent::ToolCallBoundary { toolcall_seq, .. }
            | TrimEvent::Candidate { toolcall_seq, .. } => *toolcall_seq < toolcall_seq_limit,
            TrimEvent::Cleared { .. } | TrimEvent::Snipped { .. } | TrimEvent::Sliced { .. } => {
                true
            }
        }
    }

    pub(in crate::spine) fn within_raw_boundary(&self, raw_boundary: u64) -> bool {
        if let Some((event_boundary, _)) = self.raw_live_prefix_proof() {
            return event_boundary <= raw_boundary;
        }
        match self {
            TrimEvent::Candidate { raw_ordinal, .. } => *raw_ordinal < raw_boundary,
            TrimEvent::ToolCallBoundary { .. }
            | TrimEvent::Cleared { .. }
            | TrimEvent::Snipped { .. }
            | TrimEvent::Sliced { .. } => unreachable!("raw-live proof events returned above"),
        }
    }

    pub(in crate::spine) fn toolcall_boundary_seq(&self) -> Option<u64> {
        match self {
            TrimEvent::ToolCallBoundary { toolcall_seq, .. } => Some(*toolcall_seq),
            _ => None,
        }
    }

    fn raw_live_prefix_proof(&self) -> Option<(u64, &str)> {
        match self {
            TrimEvent::ToolCallBoundary {
                raw_boundary,
                raw_live_hash,
                ..
            }
            | TrimEvent::Cleared {
                raw_boundary,
                raw_live_hash,
                ..
            }
            | TrimEvent::Snipped {
                raw_boundary,
                raw_live_hash,
                ..
            }
            | TrimEvent::Sliced {
                raw_boundary,
                raw_live_hash,
                ..
            } => Some((*raw_boundary, raw_live_hash)),
            TrimEvent::Candidate { .. } => None,
        }
    }
}
