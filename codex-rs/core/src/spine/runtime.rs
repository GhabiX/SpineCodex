use codex_protocol::models::ResponseItem;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
#[cfg(test)]
use std::ops::Range;
use thiserror::Error;

use crate::spine::archive::SpineArchive;
#[cfg(test)]
use crate::spine::io::hash_raw_live;
#[cfg(test)]
use crate::spine::model::COMMIT_MARKER_VERSION;
use crate::spine::model::ContextBaselineSource;
#[cfg(test)]
use crate::spine::model::ControlSymbol;
use crate::spine::model::LoggedPressureEvent;
use crate::spine::model::LoggedSpineLedgerEvent;
use crate::spine::model::LoggedTrimEvent;
#[cfg(test)]
use crate::spine::model::RawMask;
#[cfg(test)]
use crate::spine::model::SegRef;
use crate::spine::model::SpineCommitMarker;
#[cfg(test)]
use crate::spine::model::SpineCommitMemoryRef;
use crate::spine::model::SpineLedgerEvent;
#[cfg(test)]
use crate::spine::model::SpineTreeNode;
#[cfg(test)]
use crate::spine::model::Symbol;
#[cfg(test)]
use crate::spine::model::ToolCallSegmentKind;
use crate::spine::model::TreeMeta;
#[cfg(test)]
use crate::spine::model::TrimEvent;
#[cfg(test)]
use crate::spine::parse_stack::ParseStack;
use crate::spine::parser::ParserState;
use crate::spine::store::SpineStore;

mod accounting;
mod checkpoint;
mod close_family;
mod commit;
mod coverage;
mod host_effect;
mod load;
mod memory_artifact;
mod observe;
mod pending;
mod prepared;
mod replay;
mod root_compact;
mod session_state;
mod source_plan;
mod support;
mod trim;
mod types;

#[cfg(test)]
use crate::spine::model::commit_marker_structural_event_seqs;
pub(in crate::spine) use host_effect::SpineHistoryUpdate;
pub(in crate::spine) use host_effect::SpineHostEffects;
pub(in crate::spine) use host_effect::SpineTreeUpdateDelivery;
pub(in crate::spine) use pending::CompletedToolCall;
pub(in crate::spine) use pending::CompletedToolCallSegment;
use pending::OpenRequestAnchor;
use pending::PendingMemoryContextAccounting;
use pending::PendingToolRequest;
use pending::PendingToolResponse;
use pending::PendingTransition;
#[cfg(test)]
use pending::SpineControlToolReceipt;
pub(in crate::spine) use pending::ToolRequestAnchor;
pub(crate) use prepared::SpineCommitKind;
#[cfg(test)]
use replay::ReplayCommitClassification;
#[cfg(test)]
use replay::classify_commit_marker_for_replay;
use replay::next_event_seq_from;
use replay::next_pressure_seq_from;
use replay::next_trim_seq_from;
pub(in crate::spine) use session_state::PreparedSpineReplayRuntime;
pub(in crate::spine) use session_state::SpineCompactEvidence;
pub(in crate::spine) use session_state::SpineCompletedToolCallHostOutcome;
pub(in crate::spine) use session_state::SpineCompletedToolCallOutputEvidence;
pub(in crate::spine) use session_state::SpineInitEvidence;
pub(in crate::spine) use session_state::SpineMessageEvidence;
#[cfg(test)]
pub(crate) use session_state::SpineRootCompactHostInstall;
pub(crate) use session_state::SpineSessionState;
pub(in crate::spine) use session_state::SpineToolCallEvidence;
#[cfg(test)]
pub(crate) use session_state::SpineToolOutputRecording;
pub(in crate::spine) use session_state::SpineToolcallHookEvidence;
pub(in crate::spine) use session_state::SpineToolcallHostAttempt;
pub(in crate::spine) use session_state::SpineToolcallHostCommitAttempt;
pub(crate) use session_state::SpinetreeMemoryProjectionConfig;
pub(crate) use support::conflicting_spine_control_rejection_reason;
pub(crate) use support::is_non_toolcall_msg;
#[cfg(test)]
pub(crate) use support::is_spine_close_like_tool_name;
pub(crate) use support::is_spine_context_observation_fixed_prefix_item;
pub(crate) use support::is_spine_parser_control_tool;
pub(crate) use support::spine_mutable_context_index_for_full_history_boundary;
pub(crate) use support::spine_mutable_context_index_for_full_history_index;
pub(crate) use support::spine_tool_use_failed_message;
use support::validate_model_node_memory;
pub(crate) use types::LiveRootCompact;
pub(crate) use types::SpineCloseMemoryAssembly;
pub(crate) use types::SpineCompactSourceEntryKind;
pub(crate) use types::SpineCompactSourcePlan;
pub(crate) use types::SpineCompactSourcePlanEntry;
pub(crate) use types::SpineCurrentTrimTarget;
pub(crate) use types::SpineOpenNodeContextProjection;
pub(crate) use types::SpinePendingCloseAction;
pub(crate) use types::SpinePendingCommit;
pub(crate) use types::SpineRootCompactResult;
pub(crate) use types::SpineRootCompactTokenMetadata;
#[cfg(test)]
pub(crate) use types::SpineTokenBaselines;
pub(crate) use types::SpineTrimOutcome;
pub(crate) use types::SpineTrimUpdateOutcome;

pub(crate) const SPINE_NAMESPACE: &str = "spine";
pub(crate) const SPINE_TOOL_TREE: &str = "tree";
pub(crate) const SPINE_TOOL_OPEN: &str = "open";
pub(crate) const SPINE_TOOL_CLOSE: &str = "close";
pub(crate) const SPINE_TOOL_NEXT: &str = "next";
pub(crate) const SPINE_TOOL_TRIM: &str = "trim";

#[derive(Debug)]
pub(crate) struct SpineRuntime {
    store: SpineStore,
    ledger: SpineLedgerCache,
    parser: ParserState,
    raw_len: u64,
    raw_live: Vec<bool>,
    jit_enabled: bool,
    trim_enabled: bool,
    // Turn-local Spine control transaction state. Committed open/close effects
    // are represented by SpineLedgerEvents and ParseStack tokens; these maps
    // are empty on resume/rollback by design and are not part of h(PS).
    open_requests: BTreeMap<String, OpenRequestAnchor>,
    control_call_ids: BTreeSet<String>,
    tree_call_ids: BTreeSet<String>,
    ordinary_tool_requests: BTreeMap<String, PendingToolRequest>,
    pending_tool_responses: BTreeMap<String, Vec<PendingToolResponse>>,
    pending: Option<PendingTransition>,
    #[cfg(test)]
    control_receipts: BTreeMap<String, SpineControlToolReceipt>,
    pending_memory_context_accounting: Option<PendingMemoryContextAccounting>,
    next_user_anchor: u64,
}

#[derive(Clone, Debug)]
struct SpineLedgerCache {
    events: Vec<LoggedSpineLedgerEvent>,
    trim_events: Vec<LoggedTrimEvent>,
    next_event_seq: u64,
    next_pressure_seq: u64,
    next_trim_seq: u64,
}

impl SpineLedgerCache {
    fn new(
        events: Vec<LoggedSpineLedgerEvent>,
        pressure_events: Vec<LoggedPressureEvent>,
        trim_events: Vec<LoggedTrimEvent>,
    ) -> Result<Self, SpineError> {
        let next_event_seq = next_event_seq_from(&events)?;
        let next_pressure_seq = next_pressure_seq_from(&pressure_events)?;
        let next_trim_seq = next_trim_seq_from(&trim_events)?;
        Ok(Self {
            events,
            trim_events,
            next_event_seq,
            next_pressure_seq,
            next_trim_seq,
        })
    }

    fn retain_trim_events_at_or_before(&mut self, watermark: Option<u64>) {
        let next_trim_seq = self.next_trim_seq;
        self.trim_events
            .retain(|event| watermark.is_some_and(|watermark| event.trim_seq <= watermark));
        self.next_trim_seq = next_trim_seq;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OpenContextBaseline {
    provider_input_tokens: i64,
    source: ContextBaselineSource,
}

pub(crate) trait IntoSpineNodeMemory {
    fn into_spine_node_memory(self) -> Result<String, SpineError>;
}

impl IntoSpineNodeMemory for String {
    fn into_spine_node_memory(self) -> Result<String, SpineError> {
        validate_model_node_memory(&self)?;
        Ok(self)
    }
}

impl SpineRuntime {
    #[cfg(test)]
    pub(crate) fn current_open_index(&self) -> Result<usize, SpineError> {
        self.ensure_jit_enabled("Spine current open index")?;
        self.parser.current_open_index()
    }

    #[cfg(test)]
    pub(crate) fn current_open_input_tokens(&self) -> Option<i64> {
        self.parser.current_open_input_tokens()
    }

    fn current_close_open_meta(&self) -> Result<&TreeMeta, SpineError> {
        self.parser.current_close_open_meta()
    }

    #[cfg(test)]
    fn parse_stack(&self) -> &ParseStack {
        self.parser.parse_stack()
    }

    #[cfg(test)]
    fn parse_stack_mut_for_test(&mut self) -> &mut ParseStack {
        self.parser.parse_stack_mut_for_test()
    }

    #[cfg(test)]
    pub(crate) fn parse_stack_msg_leaf_count_for_test(&self) -> usize {
        self.parser.msg_leaf_count_for_test()
    }

    #[cfg(test)]
    pub(crate) fn parse_stack_toolcall_leaf_count_for_test(&self) -> usize {
        self.parser.toolcall_leaf_count_for_test()
    }

    #[cfg(test)]
    pub(crate) fn parse_stack_debug_for_test(&self) -> String {
        self.parser.debug_for_test()
    }

    #[cfg(test)]
    pub(crate) fn visible_response_context_refs_for_test(&self) -> Vec<(u64, usize)> {
        self.parser.visible_response_context_refs_for_test()
    }

    #[cfg(test)]
    pub(crate) fn last_visible_response_context_index_for_test(&self) -> Option<usize> {
        self.parser.last_visible_response_context_index()
    }

    fn archive(&self) -> SpineArchive {
        SpineArchive::new(self.store.root.clone())
    }

    pub(crate) fn jit_enabled(&self) -> bool {
        self.jit_enabled
    }

    pub(crate) fn set_jit_enabled(&mut self, enabled: bool) {
        self.jit_enabled = enabled;
    }

    fn ensure_jit_enabled(&self, operation: &str) -> Result<(), SpineError> {
        if self.jit_enabled {
            return Ok(());
        }
        Err(SpineError::InvalidStore(format!(
            "{operation} requires spine_jit"
        )))
    }

    fn pressure_seq_watermark(&self) -> Result<Option<u64>, SpineError> {
        Ok(self.ledger.next_pressure_seq.checked_sub(1))
    }

    fn trim_seq_watermark(&self) -> Result<Option<u64>, SpineError> {
        Ok(self.ledger.next_trim_seq.checked_sub(1))
    }

    fn append_cached_event(&mut self, event: SpineLedgerEvent) -> Result<u64, SpineError> {
        let seq = self.ledger.next_event_seq;
        let next_event_seq = seq
            .checked_add(1)
            .ok_or_else(|| SpineError::InvalidEvent("spine event seq overflow".to_string()))?;
        let logged = LoggedSpineLedgerEvent { seq, event };
        self.store.append_logged_event(&logged)?;
        self.ledger.events.push(logged);
        self.ledger.next_event_seq = next_event_seq;
        Ok(seq)
    }

    fn append_committed_events(
        &mut self,
        events: Vec<SpineLedgerEvent>,
        marker: SpineCommitMarker,
    ) -> Result<(), SpineError> {
        let seq_start = self.ledger.next_event_seq;
        if marker.token_seq_start != seq_start {
            return Err(SpineError::Invariant(format!(
                "Spine commit marker {} starts at token_seq {}, expected {seq_start}",
                marker.op_id, marker.token_seq_start
            )));
        }
        let event_count = u64::try_from(events.len())
            .map_err(|_| SpineError::InvalidEvent("spine event count overflow".to_string()))?;
        let seq_end = seq_start
            .checked_add(event_count)
            .ok_or_else(|| SpineError::InvalidEvent("spine event seq overflow".to_string()))?;
        if marker.token_seq_end != seq_end {
            return Err(SpineError::Invariant(format!(
                "Spine commit marker {} ends at token_seq {}, expected {seq_end}",
                marker.op_id, marker.token_seq_end
            )));
        }
        let logged = events
            .into_iter()
            .enumerate()
            .map(|(offset, event)| {
                let offset = u64::try_from(offset).map_err(|_| {
                    SpineError::InvalidEvent("spine event offset overflow".to_string())
                })?;
                let seq = seq_start.checked_add(offset).ok_or_else(|| {
                    SpineError::InvalidEvent("spine event seq overflow".to_string())
                })?;
                Ok(LoggedSpineLedgerEvent { seq, event })
            })
            .collect::<Result<Vec<_>, SpineError>>()?;
        for event in &logged {
            self.store.append_logged_event(event)?;
        }
        self.store.append_commit_marker(&marker)?;
        self.ledger.events.extend(logged);
        self.ledger.next_event_seq = seq_end;
        Ok(())
    }

    fn append_committed_events_no_marker(
        &mut self,
        events: Vec<SpineLedgerEvent>,
    ) -> Result<(), SpineError> {
        let seq_start = self.ledger.next_event_seq;
        let event_count = u64::try_from(events.len())
            .map_err(|_| SpineError::InvalidEvent("spine event count overflow".to_string()))?;
        let seq_end = seq_start
            .checked_add(event_count)
            .ok_or_else(|| SpineError::InvalidEvent("spine event seq overflow".to_string()))?;
        let logged = events
            .into_iter()
            .enumerate()
            .map(|(offset, event)| {
                let offset = u64::try_from(offset).map_err(|_| {
                    SpineError::InvalidEvent("spine event offset overflow".to_string())
                })?;
                let seq = seq_start.checked_add(offset).ok_or_else(|| {
                    SpineError::InvalidEvent("spine event seq overflow".to_string())
                })?;
                Ok(LoggedSpineLedgerEvent { seq, event })
            })
            .collect::<Result<Vec<_>, SpineError>>()?;
        for event in &logged {
            self.store.append_logged_event(event)?;
        }
        self.ledger.events.extend(logged);
        self.ledger.next_event_seq = seq_end;
        Ok(())
    }

    pub(crate) fn materialize_variable_context(
        &self,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<Vec<ResponseItem>, SpineError> {
        self.ensure_jit_enabled("Spine variable context materialization")?;
        let trim_projection = self.current_trim_projection()?;
        self.parser
            .materialize_variable_context(raw_items, &trim_projection)
    }

    #[cfg(test)]
    pub(crate) fn materialize_variable_context_for_test(
        &self,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<Vec<ResponseItem>, SpineError> {
        self.materialize_variable_context(raw_items)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpineErrorClass {
    ToolUse,
    Operation,
    CompactFailure,
    Invariant,
    SidecarCorruption,
    Persistence,
}

#[derive(Debug, Error)]
pub(crate) enum SpineError {
    #[error("spine tool-use error: {0}")]
    ToolUse(String),
    #[error("spine operation error: {0}")]
    Operation(String),
    #[error("spine compact error: {0}")]
    CompactFailure(String),
    #[error("spine invariant violation: {0}")]
    Invariant(String),
    #[error("spine sidecar corruption: {0}")]
    SidecarCorruption(String),
    #[error("spine store error: {0}")]
    InvalidStore(String),
    #[error("spine event error: {0}")]
    InvalidEvent(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl SpineError {
    pub(crate) fn class(&self) -> SpineErrorClass {
        match self {
            Self::ToolUse(_) => SpineErrorClass::ToolUse,
            Self::Operation(_) | Self::InvalidEvent(_) => SpineErrorClass::Operation,
            Self::CompactFailure(_) => SpineErrorClass::CompactFailure,
            Self::Invariant(_) => SpineErrorClass::Invariant,
            Self::SidecarCorruption(_) | Self::InvalidStore(_) | Self::Json(_) => {
                SpineErrorClass::SidecarCorruption
            }
            Self::Io(_) => SpineErrorClass::Persistence,
        }
    }

    pub(crate) fn should_invalidate_runtime(&self) -> bool {
        matches!(
            self.class(),
            SpineErrorClass::Invariant
                | SpineErrorClass::SidecarCorruption
                | SpineErrorClass::Persistence
        )
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
