use codex_protocol::models::ResponseItem;
use codex_protocol::spine_tree::SpineTreeUpdateEvent;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::ops::Range;
use std::path::Path;
use thiserror::Error;

use crate::spine::archive::SpineArchive;
use crate::spine::archive::StagedArchiveWrite;
use crate::spine::archive::flush_archive_writes;
use crate::spine::archive::memory_ref;
use crate::spine::archive::tree_meta_with_token_baselines;
use crate::spine::checkpoint::SpineCheckpoint;
use crate::spine::checkpoint::build_checkpoint;
use crate::spine::checkpoint::validate_checkpoint;
use crate::spine::compact_checkpoint::build_compact_checkpoint;
use crate::spine::io::hash_raw_live;
use crate::spine::io::hash_response_items;
use crate::spine::io::sha1_hex;
use crate::spine::model::COMMIT_MARKER_VERSION;
use crate::spine::model::ContextBaselineSource;
use crate::spine::model::ControlSymbol;
use crate::spine::model::LoggedPressureEvent;
use crate::spine::model::LoggedSpineLedgerEvent;
use crate::spine::model::LoggedTrimEvent;
use crate::spine::model::MemKind;
use crate::spine::model::MemRecord;
use crate::spine::model::MemoryContextAccountingRecord;
use crate::spine::model::MemoryContextAccountingSkipReason;
use crate::spine::model::MemoryContextAccountingWitnessRecord;
use crate::spine::model::NodeId;
use crate::spine::model::RawMask;
use crate::spine::model::SegRef;
use crate::spine::model::SpineCommitKindMarker;
use crate::spine::model::SpineCommitMarker;
use crate::spine::model::SpineCommitMemoryRef;
use crate::spine::model::SpineLedgerEvent;
use crate::spine::model::SpineToken;
use crate::spine::model::SpineTreeNode;
use crate::spine::model::Symbol;
use crate::spine::model::ToolCallEventSegment;
use crate::spine::model::ToolCallSegment;
use crate::spine::model::ToolCallSegmentKind;
use crate::spine::model::TreeMeta;
use crate::spine::model::TrimEvent;
use crate::spine::model::TrimProjection;
use crate::spine::model::TrimSliceSpec;
use crate::spine::parse_stack::ParseStack;
use crate::spine::parse_stack::PreparedRootEpochReduction;
use crate::spine::parse_stack::PreparedTaskTreeReduction;
use crate::spine::parse_stack::parse_stack_from_events_with_forced_events;
#[cfg(test)]
use crate::spine::parse_stack::parse_stack_msg_leaf_count;
#[cfg(test)]
use crate::spine::parse_stack::parse_stack_toolcall_leaf_count;
use crate::spine::render::VisibleItemSource;
use crate::spine::render::memory_response_item;
use crate::spine::render::project_raw_history_with_trim_projection;
use crate::spine::render::project_spine_tree_nodes_visible_items;
use crate::spine::render::read_memory_ref_body;
#[cfg(test)]
use crate::spine::render::render_parse_stack_to_context;
use crate::spine::render::render_parse_stack_to_context_with_memory_body_and_trim_projection;
use crate::spine::render::render_parse_stack_to_context_with_trim_projection;
use crate::spine::store::BODY_DIR;
use crate::spine::store::SpineStore;
use crate::spine::trimmer::Trimmer;
use crate::spine::trimmer::trim_projection_from_events;

mod replay;

#[cfg(test)]
use crate::spine::model::commit_marker_structural_event_seqs;
#[cfg(test)]
use replay::ReplayCommitClassification;
#[cfg(test)]
use replay::classify_commit_marker_for_replay;
use replay::live_context_baseline_source;
use replay::next_event_seq_from;
use replay::next_pressure_seq_from;
use replay::next_trim_seq_from;
use replay::next_user_anchor_from_events;
use replay::protocol_context_baseline_source;
use replay::replay_event_seqs_from_markers;
use replay::replay_from_events;
pub(crate) use replay::trim_projection_from_events_for_checkpoint;

pub(crate) const SPINE_NAMESPACE: &str = "spine";
pub(crate) const SPINE_TOOL_TREE: &str = "tree";
pub(crate) const SPINE_TOOL_OPEN: &str = "open";
pub(crate) const SPINE_TOOL_CLOSE: &str = "close";
pub(crate) const SPINE_TOOL_NEXT: &str = "next";
pub(crate) const SPINE_TOOL_TRIM: &str = "trim";
pub(crate) const SPINE_TOOL_FEEDBACK: &str = "feedback";
pub(crate) const SPINE_CONTROL_MULTI_CALL_REJECTION_PREFIX: &str =
    "Spine control tools are mutually exclusive within one response;";

#[derive(Debug)]
pub(crate) struct SpineRuntime {
    store: SpineStore,
    ledger: SpineLedgerCache,
    parse_stack: ParseStack,
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
    #[cfg(test)]
    pending_tool_responses: BTreeMap<String, Vec<PendingToolResponse>>,
    pending: Option<PendingTransition>,
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

#[derive(Clone, Debug)]
struct OpenRequestAnchor {
    raw_ordinal: u64,
    context_index: u64,
}

#[derive(Clone, Debug)]
struct PendingMemoryContextAccounting {
    compact_id: String,
    replacement_prefix_baseline_tokens: i64,
    close_input_tokens: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ToolRequestAnchor {
    pub(crate) raw_ordinal: u64,
    pub(crate) context_index: usize,
}

#[derive(Clone, Debug)]
enum PendingTransition {
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
    fn call_id(&self) -> &str {
        match self {
            Self::Open { call_id, .. }
            | Self::Close { call_id, .. }
            | Self::NextSugar { call_id, .. } => call_id,
        }
    }
}

#[derive(Clone, Debug)]
struct PendingMsg {
    raw_ordinal: u64,
    context_index: u64,
    from_user: bool,
    user_anchor: Option<u64>,
}

#[derive(Clone, Debug)]
struct PendingToolRequest {
    raw_ordinal: u64,
    context_index: u64,
}

#[cfg(test)]
#[derive(Clone, Debug)]
struct PendingToolResponse {
    raw_ordinal: u64,
    context_index: u64,
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

struct PreparedCloseCommit {
    suffix_start: usize,
    replacement: Vec<ResponseItem>,
    mem: MemRecord,
    memory_body: String,
    archive_writes: Vec<StagedArchiveWrite>,
    close_event: SpineLedgerEvent,
    memory: crate::spine::model::MemoryRef,
    task_tree_reduction: PreparedTaskTreeReduction,
}

struct PreparedRootCompactCommit {
    result: SpineRootCompactResult,
    mem: MemRecord,
    memory_body: String,
    compact_checkpoint: Option<crate::spine::compact_checkpoint::SpineCompactCheckpoint>,
    root_compact_event: SpineLedgerEvent,
    memory: crate::spine::model::MemoryRef,
    root_epoch_reduction: PreparedRootEpochReduction,
    next_open_index: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OpenContextBaseline {
    provider_input_tokens: i64,
    source: ContextBaselineSource,
}

enum CloseFamilyAfterClose {
    None,
    Open { summary: String },
}

struct CloseFamilyOpenPlan {
    child: NodeId,
    open_index_u64: u64,
    summary: String,
    event: SpineLedgerEvent,
}

struct CloseFamilyPlan {
    operation: &'static str,
    missing_toolcall_error: &'static str,
    event_count_underflow_error: &'static str,
    toolcall_seq_overflow_error: &'static str,
    marker_kind: SpineCommitKindMarker,
    kind: SpineCommitKind,
    toolcall_context_index: Option<usize>,
    open: Option<CloseFamilyOpenPlan>,
}

struct CloseFamilyTransaction<'a> {
    mem: &'a MemRecord,
    memory_body: &'a str,
    archive_writes: &'a [StagedArchiveWrite],
    events: Vec<SpineLedgerEvent>,
    marker_kind: SpineCommitKindMarker,
    close_event: &'a SpineLedgerEvent,
    event_count: u64,
}

enum CloseFamilyTransactionError {
    PreparedSideEffect(SpineError),
    CommitProof(SpineError),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SpineCommitKind {
    Open { open_request_index: usize },
    Close,
    CloseThenOpen { open_index: usize },
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HistoryPublicationPlan {
    operation: &'static str,
    suffix_start: usize,
    replacement_prefix: Vec<ResponseItem>,
    preserve_host_history_from: usize,
    append_current_tool_response_if_missing: bool,
}

impl HistoryPublicationPlan {
    pub(crate) fn operation(&self) -> &'static str {
        self.operation
    }

    pub(crate) fn suffix_start(&self) -> usize {
        self.suffix_start
    }

    pub(crate) fn replacement_prefix(&self) -> &[ResponseItem] {
        &self.replacement_prefix
    }

    pub(crate) fn preserve_host_history_from(&self) -> usize {
        self.preserve_host_history_from
    }

    pub(crate) fn append_current_tool_response_if_missing(&self) -> bool {
        self.append_current_tool_response_if_missing
    }
}

#[derive(Debug)]
pub(crate) struct SpinePreparedCommit {
    kind: SpineCommitKind,
    publication_plan: Option<HistoryPublicationPlan>,
    final_parse_stack: Option<ParseStack>,
    completed_toolcall: Option<CompletedToolCall>,
    toolcall_seq: Option<u64>,
    raw_items: Vec<Option<ResponseItem>>,
    mem_for_accounting: Option<MemRecord>,
}

#[derive(Debug)]
pub(crate) struct SpinePreparedRootCompact {
    result: SpineRootCompactResult,
    final_parse_stack: ParseStack,
}

impl SpinePreparedRootCompact {
    pub(crate) fn result(&self) -> &SpineRootCompactResult {
        &self.result
    }
}

impl SpinePreparedCommit {
    pub(crate) fn kind(&self) -> &SpineCommitKind {
        &self.kind
    }

    pub(crate) fn publication_plan(&self) -> Option<&HistoryPublicationPlan> {
        self.publication_plan.as_ref()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpinePendingCloseAction {
    Close,
    Next,
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

#[derive(Clone, Debug)]
pub(crate) struct SpineRootCompactResult {
    pub(crate) materialized: Vec<ResponseItem>,
    pub(crate) raw_boundary: u64,
    pub(crate) token_seq_after: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SpineTrimOutcome {
    Cleared { trim_id: String },
    AlreadyCleared { trim_id: String },
    Sliced { trim_id: String },
    Miss { trim_id: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct LiveRootCompact {
    pub(crate) raw_boundary: u64,
    pub(crate) token_seq: u64,
}

#[derive(Debug)]
pub(crate) struct SpineSessionState {
    raw_len: u64,
    runtime: Option<SpineRuntime>,
    jit_enabled: bool,
    trim_enabled: bool,
    initial_tree_snapshot_emitted: bool,
    invalid: Option<String>,
}

impl SpineSessionState {
    pub(crate) fn new() -> Self {
        Self::new_with_features(true, true)
    }

    pub(crate) fn new_with_features(jit_enabled: bool, trim_enabled: bool) -> Self {
        Self {
            raw_len: 0,
            runtime: None,
            jit_enabled,
            trim_enabled,
            initial_tree_snapshot_emitted: false,
            invalid: None,
        }
    }

    pub(crate) fn runtime(&self) -> Option<&SpineRuntime> {
        if self.invalid.is_some() {
            return None;
        }
        self.runtime.as_ref()
    }

    pub(crate) fn runtime_mut(&mut self) -> Option<&mut SpineRuntime> {
        if self.invalid.is_some() {
            return None;
        }
        self.runtime.as_mut()
    }

    pub(crate) fn is_ready(&self) -> bool {
        self.invalid.is_none() && self.runtime.is_some()
    }

    pub(crate) fn raw_len(&self) -> u64 {
        self.raw_len
    }

    pub(crate) fn set_replayed(
        &mut self,
        raw_len: u64,
        mut runtime: Option<SpineRuntime>,
    ) -> Result<(), SpineError> {
        drop(self.runtime.take());
        if let Some(runtime) = runtime.as_mut() {
            runtime.set_jit_enabled(self.jit_enabled);
            runtime.set_trim_enabled(self.trim_enabled);
            runtime.acquire_writer_lock()?;
        }
        self.raw_len = raw_len;
        self.runtime = runtime;
        self.initial_tree_snapshot_emitted = false;
        self.invalid = None;
        Ok(())
    }

    pub(crate) fn invalidate(&mut self, reason: impl Into<String>) {
        self.invalid = Some(reason.into());
    }

    pub(crate) fn release_runtime_for_shutdown(&mut self) {
        self.runtime = None;
    }

    pub(crate) fn release_runtime_for_replay(&mut self) {
        self.runtime = None;
        self.initial_tree_snapshot_emitted = false;
    }

    pub(crate) fn abort_pending_tool(&mut self, call_id: &str) -> bool {
        let Some(runtime) = self.runtime_mut() else {
            return false;
        };
        runtime.abort_pending(call_id)
    }

    pub(crate) fn abort_any_pending(&mut self) -> Option<String> {
        let runtime = self.runtime_mut()?;
        runtime.abort_any_pending()
    }

    fn invalid_error(&self) -> Option<SpineError> {
        self.invalid
            .as_ref()
            .map(|reason| SpineError::Invariant(format!("spine runtime is invalid: {reason}")))
    }

    pub(crate) fn ensure_valid(&self) -> Result<(), SpineError> {
        if let Some(err) = self.invalid_error() {
            return Err(err);
        }
        Ok(())
    }

    pub(crate) fn observe_raw_items(&mut self, count: usize) -> Result<(), SpineError> {
        if let Some(err) = self.invalid_error() {
            return Err(err);
        }
        let count = u64::try_from(count)
            .map_err(|_| SpineError::InvalidEvent("raw item count overflow".to_string()))?;
        self.raw_len = self
            .raw_len
            .checked_add(count)
            .ok_or_else(|| SpineError::InvalidEvent("raw ordinal overflow".to_string()))?;
        if let Some(runtime) = self.runtime.as_mut() {
            let count = usize::try_from(count)
                .map_err(|_| SpineError::InvalidEvent("raw item count overflow".to_string()))?;
            runtime.observe_raw_items(count)?;
        }
        Ok(())
    }

    pub(crate) fn ensure_runtime(&mut self, rollout_path: &Path) -> Result<(), SpineError> {
        if let Some(err) = self.invalid_error() {
            return Err(err);
        }
        if self.runtime.is_none() {
            let mut runtime = SpineRuntime::load_or_create_with_jit(
                rollout_path,
                self.raw_len,
                self.jit_enabled,
            )?;
            runtime.set_jit_enabled(self.jit_enabled);
            runtime.set_trim_enabled(self.trim_enabled);
            self.runtime = Some(runtime);
        }
        Ok(())
    }

    pub(crate) fn take_initial_tree_snapshot(
        &mut self,
    ) -> Result<Option<SpineTreeUpdateEvent>, SpineError> {
        if let Some(err) = self.invalid_error() {
            return Err(err);
        }
        if self.initial_tree_snapshot_emitted {
            return Ok(None);
        }
        let Some(runtime) = self.runtime.as_ref() else {
            return Ok(None);
        };
        if !runtime.jit_enabled() {
            return Ok(None);
        }
        let snapshot = runtime.build_tree_snapshot()?;
        self.initial_tree_snapshot_emitted = true;
        Ok(Some(snapshot))
    }
}

impl SpineRuntime {
    pub(crate) fn load_or_create(rollout_path: &Path, raw_len: u64) -> Result<Self, SpineError> {
        Self::load_or_create_with_jit(rollout_path, raw_len, true)
    }

    pub(crate) fn load_or_create_with_jit(
        rollout_path: &Path,
        raw_len: u64,
        jit_enabled: bool,
    ) -> Result<Self, SpineError> {
        let store = SpineStore::load_or_create_for_writer(rollout_path)?;
        if !jit_enabled {
            let raw_len_usize = usize::try_from(raw_len)
                .map_err(|_| SpineError::InvalidEvent("raw item count overflow".to_string()))?;
            return Self::load_trim_only(store, vec![true; raw_len_usize]);
        }
        if jit_enabled && !store.tree_path().exists() {
            store.append_event(&SpineLedgerEvent::Init { raw_start: 0 })?;
            store.append_event(&SpineLedgerEvent::Open {
                child: NodeId::root_epoch(1).child(1),
                boundary: raw_len,
                index: raw_len,
                summary: "root".to_string(),
                open_input_tokens: None,
                open_context_tokens: None,
                open_context_source: None,
            })?;
        }
        let mut runtime = Self::load(store, raw_len)?;
        runtime.set_jit_enabled(jit_enabled);
        Ok(runtime)
    }

    fn load_trim_only(store: SpineStore, raw_live: Vec<bool>) -> Result<Self, SpineError> {
        let ledger = SpineLedgerCache::new(Vec::new(), Vec::new(), store.trim_events()?)?;
        let next_user_anchor = next_user_anchor_from_events(&ledger.events)?;
        Ok(Self {
            store,
            ledger,
            parse_stack: ParseStack::new(),
            raw_len: u64::try_from(raw_live.len())
                .map_err(|_| SpineError::InvalidEvent("raw item count overflow".to_string()))?,
            raw_live,
            jit_enabled: false,
            trim_enabled: true,
            open_requests: BTreeMap::new(),
            control_call_ids: BTreeSet::new(),
            tree_call_ids: BTreeSet::new(),
            ordinary_tool_requests: BTreeMap::new(),
            #[cfg(test)]
            pending_tool_responses: BTreeMap::new(),
            pending: None,
            pending_memory_context_accounting: None,
            next_user_anchor,
        })
    }

    pub(crate) fn load_for_rollout_items(
        rollout_path: &Path,
        raw_items: &[Option<ResponseItem>],
        rollback_cuts: &[usize],
    ) -> Result<Option<Self>, SpineError> {
        if !SpineStore::has_for_rollout(rollout_path)? {
            return Ok(None);
        }
        let runtime = Self::load_for_rollout_items_from_store(
            SpineStore::for_rollout(rollout_path)?,
            rollout_path,
            raw_items,
            rollback_cuts,
        )?;
        Ok(Some(runtime))
    }

    pub(crate) fn load_for_rollout_items_for_writer(
        rollout_path: &Path,
        raw_items: &[Option<ResponseItem>],
        rollback_cuts: &[usize],
    ) -> Result<Option<Self>, SpineError> {
        Self::load_for_rollout_items_for_writer_with_jit(
            rollout_path,
            raw_items,
            rollback_cuts,
            true,
        )
    }

    pub(crate) fn load_for_rollout_items_for_writer_with_jit(
        rollout_path: &Path,
        raw_items: &[Option<ResponseItem>],
        rollback_cuts: &[usize],
        jit_enabled: bool,
    ) -> Result<Option<Self>, SpineError> {
        if !SpineStore::has_for_rollout(rollout_path)? {
            return Ok(None);
        }
        if !jit_enabled {
            if !rollback_cuts.is_empty() {
                return Err(SpineError::InvalidStore(
                    "spine_trim-only replay does not support rollback cuts".to_string(),
                ));
            }
            let raw_live = raw_items.iter().map(Option::is_some).collect();
            let runtime = Self::load_trim_only(
                SpineStore::for_rollout(rollout_path)?.with_writer_lock()?,
                raw_live,
            )?;
            return Ok(Some(runtime));
        }
        let runtime = Self::load_for_rollout_items_from_store(
            SpineStore::for_rollout(rollout_path)?.with_writer_lock()?,
            rollout_path,
            raw_items,
            rollback_cuts,
        )?;
        Ok(Some(runtime))
    }

    fn load_for_rollout_items_from_store(
        store: SpineStore,
        rollout_path: &Path,
        raw_items: &[Option<ResponseItem>],
        rollback_cuts: &[usize],
    ) -> Result<Self, SpineError> {
        let runtime = Self::load_with_raw_live_for_rollout(
            store,
            raw_items.iter().map(Option::is_some).collect(),
            rollback_cuts,
            rollout_path,
            raw_items,
        )?;
        runtime.validate_raw_coverage(raw_items)?;
        Ok(runtime)
    }

    #[cfg(test)]
    pub(crate) fn load_for_rollout(
        rollout_path: &Path,
        raw_len: u64,
    ) -> Result<Option<Self>, SpineError> {
        if !SpineStore::has_for_rollout(rollout_path)? {
            return Ok(None);
        }
        Self::load(SpineStore::for_rollout(rollout_path)?, raw_len).map(Some)
    }

    pub(crate) fn load(store: SpineStore, raw_len: u64) -> Result<Self, SpineError> {
        let raw_len_usize = usize::try_from(raw_len)
            .map_err(|_| SpineError::InvalidEvent("raw item count overflow".to_string()))?;
        Self::load_with_raw_live(store, vec![true; raw_len_usize])
    }

    pub(crate) fn acquire_writer_lock(&mut self) -> Result<(), SpineError> {
        self.store.ensure_writer_lock()
    }

    fn load_with_raw_live(store: SpineStore, raw_live: Vec<bool>) -> Result<Self, SpineError> {
        Self::load_with_raw_live_and_event_limit(store, raw_live, None)
    }

    fn load_with_raw_live_for_rollout(
        store: SpineStore,
        raw_live: Vec<bool>,
        rollback_cuts: &[usize],
        rollout_path: &Path,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<Self, SpineError> {
        let checkpoint = store.rollback_checkpoint(rollback_cuts)?;
        let trim_events = store.trim_events()?;
        if let Some(checkpoint) = checkpoint.as_ref() {
            validate_checkpoint(checkpoint, rollout_path, &raw_live, raw_items, &trim_events)?;
            return Self::load_with_rollback_checkpoint(store, raw_live, checkpoint);
        }
        if let Some(checkpoint) = store.resume_checkpoint(raw_live.len())? {
            validate_checkpoint(
                &checkpoint,
                rollout_path,
                &raw_live,
                raw_items,
                &trim_events,
            )?;
            Self::validate_checkpoint_parse_stack_prefix(&store, &raw_live, &checkpoint)?;
        }
        Self::load_with_raw_live(store, raw_live)
    }

    fn validate_checkpoint_parse_stack_prefix(
        store: &SpineStore,
        raw_live: &[bool],
        checkpoint: &SpineCheckpoint,
    ) -> Result<(), SpineError> {
        let ledger = SpineLedgerCache::new(
            store.events()?,
            store.pressure_events()?,
            store.trim_events()?,
        )?;
        let mems = store.mems()?;
        let markers = store.commit_markers()?;
        store.validate_commit_markers_for_replay(
            &ledger.events,
            &mems,
            raw_live,
            None,
            Some(checkpoint.token_seq),
        )?;
        let archive = SpineArchive::new(store.root.clone());
        let raw_ordinal = usize::try_from(checkpoint.raw_ordinal)
            .map_err(|_| SpineError::InvalidEvent("checkpoint raw ordinal overflow".to_string()))?;
        let prefix_live = &raw_live[..raw_ordinal.min(raw_live.len())];
        let prefix_mask = RawMask::new(prefix_live);
        let prefix_events = ledger
            .events
            .iter()
            .filter(|event| event.seq < checkpoint.token_seq)
            .cloned()
            .collect::<Vec<_>>();
        let prefix_replay_event_seqs = replay_event_seqs_from_markers(
            &ledger.events,
            &markers,
            &mems,
            prefix_mask,
            None,
            Some(checkpoint.token_seq),
            true,
        )?;
        let prefix_ps = parse_stack_from_events_with_forced_events(
            &prefix_events,
            &archive,
            &mems,
            prefix_mask,
            &prefix_replay_event_seqs.forced,
            &prefix_replay_event_seqs.marker_structural,
        )?;
        if prefix_ps != checkpoint.parse_stack {
            return Err(SpineError::Invariant(format!(
                "spine checkpoint ParseStack mismatch for {} at raw_ordinal={} token_seq={}",
                checkpoint.checkpoint_id, checkpoint.raw_ordinal, checkpoint.token_seq
            )));
        }
        Ok(())
    }

    fn load_with_raw_live_and_event_limit(
        store: SpineStore,
        raw_live: Vec<bool>,
        event_limit: Option<u64>,
    ) -> Result<Self, SpineError> {
        let ledger = SpineLedgerCache::new(
            store.events()?,
            store.pressure_events()?,
            store.trim_events()?,
        )?;
        let next_user_anchor = next_user_anchor_from_events(&ledger.events)?;
        let mems = store.mems()?;
        let markers = store.commit_markers()?;
        store.validate_commit_markers_for_replay(
            &ledger.events,
            &mems,
            &raw_live,
            None,
            event_limit,
        )?;
        let replay_event_seqs = replay_event_seqs_from_markers(
            &ledger.events,
            &markers,
            &mems,
            RawMask::new(&raw_live),
            None,
            event_limit,
            true,
        )?;
        let events = if let Some(limit) = event_limit {
            ledger
                .events
                .iter()
                .filter(|event| event.seq < limit)
                .cloned()
                .collect::<Vec<_>>()
        } else {
            ledger.events.clone()
        };
        let archive = SpineArchive::new(store.root.clone());
        let parse_stack = replay_from_events(
            &archive,
            &events,
            &mems,
            &raw_live,
            &replay_event_seqs,
            None,
            None,
        )?;
        let pending_memory_context_accounting =
            pending_memory_context_accounting_from_store(&store)?;
        Ok(Self {
            store,
            ledger,
            parse_stack,
            raw_len: u64::try_from(raw_live.len())
                .map_err(|_| SpineError::InvalidEvent("raw item count overflow".to_string()))?,
            raw_live,
            jit_enabled: true,
            trim_enabled: true,
            open_requests: BTreeMap::new(),
            control_call_ids: BTreeSet::new(),
            tree_call_ids: BTreeSet::new(),
            ordinary_tool_requests: BTreeMap::new(),
            #[cfg(test)]
            pending_tool_responses: BTreeMap::new(),
            pending: None,
            pending_memory_context_accounting,
            next_user_anchor,
        })
    }

    fn load_with_rollback_checkpoint(
        store: SpineStore,
        raw_live: Vec<bool>,
        checkpoint: &SpineCheckpoint,
    ) -> Result<Self, SpineError> {
        let mut ledger = SpineLedgerCache::new(
            store.events()?,
            store.pressure_events()?,
            store.trim_events()?,
        )?;
        let next_user_anchor = next_user_anchor_from_events(&ledger.events)?;
        ledger.retain_trim_events_at_or_before(checkpoint.trim_seq_watermark);
        let mems = store.mems()?;
        let markers = store.commit_markers()?;
        store.validate_commit_markers_for_replay(
            &ledger.events,
            &mems,
            &raw_live,
            Some(checkpoint.token_seq),
            None,
        )?;
        let replay_event_seqs = replay_event_seqs_from_markers(
            &ledger.events,
            &markers,
            &mems,
            RawMask::new(&raw_live),
            Some(checkpoint.token_seq),
            None,
            false,
        )?;
        let archive = SpineArchive::new(store.root.clone());
        let raw_ordinal = usize::try_from(checkpoint.raw_ordinal)
            .map_err(|_| SpineError::InvalidEvent("checkpoint raw ordinal overflow".to_string()))?;
        let prefix_live = &raw_live[..raw_ordinal.min(raw_live.len())];
        let prefix_mask = RawMask::new(prefix_live);
        let prefix_events = ledger
            .events
            .iter()
            .filter(|event| event.seq < checkpoint.token_seq)
            .cloned()
            .collect::<Vec<_>>();
        let prefix_replay_event_seqs = replay_event_seqs_from_markers(
            &ledger.events,
            &markers,
            &mems,
            prefix_mask,
            None,
            Some(checkpoint.token_seq),
            true,
        )?;
        let prefix_ps = parse_stack_from_events_with_forced_events(
            &prefix_events,
            &archive,
            &mems,
            prefix_mask,
            &prefix_replay_event_seqs.forced,
            &prefix_replay_event_seqs.marker_structural,
        )?;
        if prefix_ps != checkpoint.parse_stack {
            return Err(SpineError::Invariant(format!(
                "spine checkpoint ParseStack mismatch for {} at raw_ordinal={} token_seq={}",
                checkpoint.checkpoint_id, checkpoint.raw_ordinal, checkpoint.token_seq
            )));
        }

        let parse_stack = replay_from_events(
            &archive,
            &ledger.events,
            &mems,
            &raw_live,
            &replay_event_seqs,
            Some(&checkpoint.parse_stack),
            Some(checkpoint.token_seq),
        )?;
        let pending_memory_context_accounting =
            pending_memory_context_accounting_from_store(&store)?;
        Ok(Self {
            store,
            ledger,
            parse_stack,
            raw_len: u64::try_from(raw_live.len())
                .map_err(|_| SpineError::InvalidEvent("raw item count overflow".to_string()))?,
            raw_live,
            jit_enabled: true,
            trim_enabled: true,
            open_requests: BTreeMap::new(),
            control_call_ids: BTreeSet::new(),
            tree_call_ids: BTreeSet::new(),
            ordinary_tool_requests: BTreeMap::new(),
            #[cfg(test)]
            pending_tool_responses: BTreeMap::new(),
            pending: None,
            pending_memory_context_accounting,
            next_user_anchor,
        })
    }

    #[cfg(test)]
    pub(crate) fn render_tree(&self) -> Result<String, SpineError> {
        self.parse_stack_with_memory_context_accounting()?
            .render_tree()
    }

    pub(crate) fn render_tree_with_context_annotations(
        &self,
        annotations: &BTreeMap<NodeId, String>,
    ) -> Result<String, SpineError> {
        self.ensure_jit_enabled("Spine tree render")?;
        self.parse_stack_with_memory_context_accounting()?
            .render_tree_with_context_annotations(annotations)
    }

    pub(crate) fn build_tree_snapshot(&self) -> Result<SpineTreeUpdateEvent, SpineError> {
        self.ensure_jit_enabled("Spine tree snapshot")?;
        let parse_stack = self.parse_stack_with_memory_context_accounting()?;
        let nodes = parse_stack.tree_snapshot_nodes()?;
        let active_node_id = parse_stack.current_cursor_id()?.as_path();
        Ok(SpineTreeUpdateEvent {
            snapshot_seq: self.ledger.next_event_seq,
            active_node_id,
            nodes,
            planned_nodes: Vec::new(),
        })
    }

    fn parse_stack_with_memory_context_accounting(&self) -> Result<ParseStack, SpineError> {
        let accounting = self.memory_context_accounting_by_id()?;
        let mut parse_stack = self.parse_stack.clone();
        parse_stack.apply_memory_context_accounting(&accounting);
        Ok(parse_stack)
    }

    fn memory_context_accounting_by_id(&self) -> Result<BTreeMap<String, i64>, SpineError> {
        let mut out = BTreeMap::new();
        for record in self.store.mem_accounting()? {
            match out.get(&record.compact_id).copied() {
                Some(existing) if existing != record.closed_memory_context_tokens => {
                    return Err(SpineError::InvalidStore(format!(
                        "conflicting Spine memory context accounting for {}",
                        record.compact_id
                    )));
                }
                Some(_) => {}
                None => {
                    out.insert(record.compact_id, record.closed_memory_context_tokens);
                }
            }
        }
        Ok(out)
    }

    pub(crate) fn append_feedback_markdown(&self, entry: &str) -> Result<(), SpineError> {
        self.store.append_feedback_markdown(entry)
    }

    pub(crate) fn current_open_index(&self) -> Result<usize, SpineError> {
        self.ensure_jit_enabled("Spine current open index")?;
        Ok(self.parse_stack.current_open_meta()?.index)
    }

    #[cfg(test)]
    pub(crate) fn current_open_input_tokens(&self) -> Option<i64> {
        self.parse_stack
            .current_open_meta_opt()
            .and_then(|meta| meta.open_input_tokens)
    }

    #[cfg(test)]
    pub(crate) fn current_open_provider_input_tokens(&self) -> Option<i64> {
        self.current_open_context_baseline()
            .map(|baseline| baseline.provider_input_tokens)
    }

    #[cfg(test)]
    pub(crate) fn current_open_context_baseline_source(
        &self,
    ) -> Option<codex_protocol::spine_tree::SpineNodeContextBaselineSource> {
        self.current_open_context_baseline()
            .map(|baseline| baseline.source)
            .map(protocol_context_baseline_source)
    }

    #[cfg(test)]
    fn current_open_context_baseline(&self) -> Option<OpenContextBaseline> {
        self.parse_stack
            .current_open_meta_opt()
            .and_then(|meta| self.open_context_baseline_for(meta).ok().flatten())
    }

    pub(crate) fn open_node_context_projections(&self) -> Vec<SpineOpenNodeContextProjection> {
        if !self.jit_enabled {
            return Vec::new();
        }
        self.parse_stack
            .live_open_metas()
            .into_iter()
            .map(|meta| {
                let (baseline, problem) = match self.open_context_baseline_for(meta) {
                    Ok(baseline) => (baseline, None),
                    Err(problem) => (None, Some(problem)),
                };
                SpineOpenNodeContextProjection {
                    node_id: meta.id.clone(),
                    provider_input_tokens: baseline.map(|baseline| baseline.provider_input_tokens),
                    baseline_source: baseline
                        .map(|baseline| baseline.source)
                        .map(protocol_context_baseline_source),
                    problem,
                }
            })
            .collect()
    }

    fn open_context_baseline_for(
        &self,
        meta: &TreeMeta,
    ) -> Result<Option<OpenContextBaseline>, codex_protocol::spine_tree::SpineNodeContextProblem>
    {
        let source = match live_context_baseline_source(
            meta.open_context_source
                .unwrap_or(ContextBaselineSource::ProviderAtOpen),
        ) {
            Some(source) => source,
            None => return Ok(None),
        };
        match (meta.open_input_tokens, meta.open_context_tokens) {
            (Some(provider_input_tokens), Some(open_context_tokens))
                if provider_input_tokens == open_context_tokens =>
            {
                Ok(Some(OpenContextBaseline {
                    provider_input_tokens,
                    source,
                }))
            }
            (None, None) => Ok(None),
            (Some(_), Some(_)) | (Some(_), None) | (None, Some(_)) => {
                Err(codex_protocol::spine_tree::SpineNodeContextProblem::CorruptPressureMetadata)
            }
        }
    }

    pub(crate) fn capture_current_open_provider_baseline(
        &mut self,
        input_tokens: i64,
    ) -> Result<bool, SpineError> {
        if !self.jit_enabled || input_tokens <= 0 {
            return Ok(false);
        }
        let open_meta = match self.parse_stack.current_open_meta_opt() {
            Some(meta) => meta.clone(),
            None => return Ok(false),
        };
        if open_meta.open_context_tokens.is_some() {
            return Ok(false);
        }
        if !self.current_open_accepts_deferred_provider_baseline(&open_meta)? {
            return Ok(false);
        }
        let event = SpineLedgerEvent::OpenContextBaseline {
            node: open_meta.id.clone(),
            raw_boundary: self.raw_len,
            raw_live_hash: hash_raw_live(&self.raw_live),
            open_input_tokens: input_tokens,
            open_context_tokens: input_tokens,
            open_context_source: ContextBaselineSource::ProviderAtOpen,
        };
        self.append_cached_event(event)?;
        self.parse_stack.set_live_open_context_baseline(
            &open_meta.id,
            input_tokens,
            ContextBaselineSource::ProviderAtOpen,
        )
    }

    pub(crate) fn capture_closed_memory_context_accounting(
        &mut self,
        provider_input_tokens: i64,
    ) -> Result<bool, SpineError> {
        if !self.jit_enabled {
            return Ok(false);
        }
        let Some(pending) = self.pending_memory_context_accounting.take() else {
            return Ok(false);
        };
        if provider_input_tokens <= 0 {
            self.consume_memory_context_accounting_pending(
                pending,
                None,
                MemoryContextAccountingSkipReason::MissingProviderUsage,
            )?;
            return Ok(false);
        }
        if self
            .memory_context_accounting_by_id()?
            .contains_key(&pending.compact_id)
        {
            self.consume_memory_context_accounting_pending(
                pending,
                Some(provider_input_tokens),
                MemoryContextAccountingSkipReason::InvalidProviderUsage,
            )?;
            return Ok(false);
        }
        if let Some(close_input_tokens) = pending.close_input_tokens
            && provider_input_tokens >= close_input_tokens
        {
            self.consume_memory_context_accounting_pending(
                pending,
                Some(provider_input_tokens),
                MemoryContextAccountingSkipReason::InvalidProviderUsage,
            )?;
            return Ok(false);
        }
        let memory_tokens = provider_input_tokens - pending.replacement_prefix_baseline_tokens;
        if memory_tokens < 0 {
            self.consume_memory_context_accounting_pending(
                pending,
                Some(provider_input_tokens),
                MemoryContextAccountingSkipReason::NegativeMemoryDelta,
            )?;
            return Ok(false);
        }
        self.store
            .append_mem_accounting(&MemoryContextAccountingRecord {
                compact_id: pending.compact_id.clone(),
                closed_memory_context_tokens: memory_tokens,
                provider_input_tokens,
                replacement_prefix_baseline_tokens: pending.replacement_prefix_baseline_tokens,
            })?;
        Ok(true)
    }

    pub(crate) fn consume_closed_memory_context_accounting_without_provider_usage(
        &mut self,
    ) -> Result<bool, SpineError> {
        if !self.jit_enabled {
            return Ok(false);
        }
        let Some(pending) = self.pending_memory_context_accounting.take() else {
            return Ok(false);
        };
        self.consume_memory_context_accounting_pending(
            pending,
            None,
            MemoryContextAccountingSkipReason::MissingProviderUsage,
        )?;
        Ok(true)
    }

    fn consume_memory_context_accounting_pending(
        &self,
        pending: PendingMemoryContextAccounting,
        provider_input_tokens: Option<i64>,
        reason: MemoryContextAccountingSkipReason,
    ) -> Result<(), SpineError> {
        self.store
            .append_mem_accounting_witness(&MemoryContextAccountingWitnessRecord::Consumed {
                compact_id: pending.compact_id,
                provider_input_tokens,
                reason,
            })
    }

    fn append_memory_context_accounting_pending(
        &mut self,
        pending: PendingMemoryContextAccounting,
    ) -> Result<(), SpineError> {
        if let Some(existing) = self.pending_memory_context_accounting.take() {
            self.consume_memory_context_accounting_pending(
                existing,
                None,
                MemoryContextAccountingSkipReason::SupersededByNewPending,
            )?;
        }
        if self
            .memory_context_accounting_by_id()?
            .contains_key(&pending.compact_id)
        {
            return Ok(());
        }
        self.store.append_mem_accounting_witness(
            &MemoryContextAccountingWitnessRecord::Pending {
                compact_id: pending.compact_id.clone(),
                replacement_prefix_baseline_tokens: pending.replacement_prefix_baseline_tokens,
                close_input_tokens: pending.close_input_tokens,
            },
        )?;
        self.pending_memory_context_accounting = Some(pending);
        Ok(())
    }

    fn current_open_accepts_deferred_provider_baseline(
        &self,
        open_meta: &TreeMeta,
    ) -> Result<bool, SpineError> {
        if open_meta.summary == "root"
            && open_meta
                .id
                .parent()
                .is_some_and(|parent| parent.is_root_epoch())
        {
            return Ok(true);
        }
        let Some(open_seq) = self
            .ledger
            .events
            .iter()
            .rev()
            .find_map(|event| match &event.event {
                SpineLedgerEvent::Open { child, .. } if child == &open_meta.id => Some(event.seq),
                _ => None,
            })
        else {
            return Ok(false);
        };
        Ok(self.store.commit_markers()?.iter().any(|marker| {
            marker.kind == SpineCommitKindMarker::CloseThenOpen
                && marker
                    .token_seq_start
                    .checked_add(1)
                    .is_some_and(|seq| seq == open_seq)
        }))
    }

    fn current_close_open_meta(&self) -> Result<&TreeMeta, SpineError> {
        let Some(open_meta) = self.parse_stack.current_open_meta_opt() else {
            let cursor = self.parse_stack.current_cursor_id()?;
            if cursor.is_root_epoch() {
                return Err(SpineError::Operation(format!(
                    "cannot close root epoch cursor {cursor}"
                )));
            }
            return Err(SpineError::Operation(
                "spine.close requires a live open task".to_string(),
            ));
        };
        if open_meta.id.is_root_epoch() {
            return Err(SpineError::Operation("cannot close root epoch".to_string()));
        }
        Ok(open_meta)
    }

    #[cfg(test)]
    fn parse_stack(&self) -> &ParseStack {
        &self.parse_stack
    }

    #[cfg(test)]
    pub(crate) fn parse_stack_msg_leaf_count_for_test(&self) -> usize {
        parse_stack_msg_leaf_count(&self.parse_stack.symbols)
    }

    #[cfg(test)]
    pub(crate) fn parse_stack_toolcall_leaf_count_for_test(&self) -> usize {
        parse_stack_toolcall_leaf_count(&self.parse_stack.symbols)
    }

    #[cfg(test)]
    pub(crate) fn parse_stack_debug_for_test(&self) -> String {
        format!("{:?}", self.parse_stack)
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

    pub(crate) fn set_trim_enabled(&mut self, enabled: bool) {
        self.trim_enabled = enabled;
    }

    fn trimmer(&mut self) -> Trimmer<'_> {
        Trimmer::new(
            &self.store,
            &mut self.ledger.trim_events,
            &mut self.ledger.next_trim_seq,
            self.raw_len,
            &self.raw_live,
            self.trim_enabled,
        )
    }

    fn current_trim_structural_seq(&self) -> u64 {
        if self.jit_enabled {
            self.ledger.next_event_seq
        } else {
            self.ledger.next_trim_seq
        }
    }

    pub(crate) fn observe_raw_items(&mut self, count: usize) -> Result<(), SpineError> {
        let count = u64::try_from(count)
            .map_err(|_| SpineError::InvalidEvent("raw item count overflow".to_string()))?;
        self.raw_len = self
            .raw_len
            .checked_add(count)
            .ok_or_else(|| SpineError::InvalidEvent("raw ordinal overflow".to_string()))?;
        let count = usize::try_from(count)
            .map_err(|_| SpineError::InvalidEvent("raw item count overflow".to_string()))?;
        self.raw_live.extend(std::iter::repeat_n(true, count));
        Ok(())
    }

    pub(crate) fn observe_context_item(
        &mut self,
        raw_ordinal: u64,
        context_index: usize,
        item: &ResponseItem,
    ) -> Result<(), SpineError> {
        if !self.jit_enabled {
            return Ok(());
        }
        let context_index = u64::try_from(context_index)
            .map_err(|_| SpineError::InvalidEvent("context index overflow".to_string()))?;
        let from_user = is_real_user_message(item);
        let user_anchor = if from_user {
            let user_anchor = self.next_user_anchor;
            self.next_user_anchor = self
                .next_user_anchor
                .checked_add(1)
                .ok_or_else(|| SpineError::InvalidEvent("user anchor overflow".to_string()))?;
            Some(user_anchor)
        } else {
            None
        };
        let msg = PendingMsg {
            raw_ordinal,
            context_index,
            from_user,
            user_anchor,
        };
        if let ResponseItem::FunctionCall {
            call_id,
            name,
            namespace: Some(namespace),
            ..
        } = item
            && namespace == SPINE_NAMESPACE
            && is_spine_parser_control_tool_name(name)
        {
            self.control_call_ids.insert(call_id.clone());
            if name == SPINE_TOOL_OPEN && self.open_requests.contains_key(call_id) {
                return Err(SpineError::InvalidEvent(format!(
                    "duplicate spine.open request anchor for {call_id}"
                )));
            }
            if self.ordinary_tool_requests.contains_key(call_id) {
                return Err(SpineError::InvalidEvent(format!(
                    "duplicate tool request anchor for {call_id}"
                )));
            }
            self.ordinary_tool_requests.insert(
                call_id.clone(),
                PendingToolRequest {
                    raw_ordinal: msg.raw_ordinal,
                    context_index: msg.context_index,
                },
            );
            if name == SPINE_TOOL_OPEN {
                self.open_requests.insert(
                    call_id.clone(),
                    OpenRequestAnchor {
                        raw_ordinal: msg.raw_ordinal,
                        context_index: msg.context_index,
                    },
                );
            }
            return Ok(());
        }
        if let ResponseItem::FunctionCall {
            call_id,
            name,
            namespace: Some(namespace),
            ..
        } = item
            && namespace == SPINE_NAMESPACE
            && name == SPINE_TOOL_TREE
        {
            self.tree_call_ids.insert(call_id.clone());
            if self.ordinary_tool_requests.contains_key(call_id) {
                return Err(SpineError::InvalidEvent(format!(
                    "duplicate tool request anchor for {call_id}"
                )));
            }
            self.ordinary_tool_requests.insert(
                call_id.clone(),
                PendingToolRequest {
                    raw_ordinal: msg.raw_ordinal,
                    context_index: msg.context_index,
                },
            );
            return Ok(());
        }
        if let Some(call_id) = tool_request_call_id(item) {
            if self.ordinary_tool_requests.contains_key(call_id) {
                return Err(SpineError::InvalidEvent(format!(
                    "duplicate ordinary tool request anchor for {call_id}"
                )));
            }
            self.ordinary_tool_requests.insert(
                call_id.to_string(),
                PendingToolRequest {
                    raw_ordinal: msg.raw_ordinal,
                    context_index: msg.context_index,
                },
            );
            return Ok(());
        }
        if let Some(call_id) = tool_response_call_id(item) {
            #[cfg(test)]
            self.pending_tool_responses
                .entry(call_id.to_string())
                .or_default()
                .push(PendingToolResponse {
                    raw_ordinal: msg.raw_ordinal,
                    context_index: msg.context_index,
                });
            if self.control_call_ids.contains(call_id)
                || self.tree_call_ids.remove(call_id)
                || self
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.call_id() == call_id)
            {
                return Ok(());
            }
            return Ok(());
        }
        if matches!(
            item,
            ResponseItem::ToolSearchOutput { call_id: None, .. }
                | ResponseItem::ToolSearchCall { call_id: None, .. }
        ) {
            return Ok(());
        }
        self.append_and_shift_msg(&msg)
    }

    pub(crate) fn observe_completed_toolcall(
        &mut self,
        toolcall: CompletedToolCall,
    ) -> Result<(), SpineError> {
        self.observe_completed_toolcall_with_raw_items(toolcall, &[])
    }

    pub(crate) fn observe_completed_toolcall_with_raw_items(
        &mut self,
        toolcall: CompletedToolCall,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<(), SpineError> {
        if !self.jit_enabled {
            return self.observe_completed_toolcall_for_trim(toolcall, raw_items);
        }
        let (event, segments) = self.completed_toolcall_parts(&toolcall)?;
        let toolcall_seq = self.append_cached_event(event)?;
        self.push_completed_toolcall_token(segments)?;
        self.append_trim_candidates_for_completed_toolcall(&toolcall, toolcall_seq, raw_items)?;
        self.clear_completed_toolcall_anchors(&toolcall);
        Ok(())
    }

    pub(crate) fn abort_pending_and_observe_completed_toolcall(
        &mut self,
        call_id: &str,
        toolcall: CompletedToolCall,
    ) -> Result<bool, SpineError> {
        self.abort_pending_and_observe_completed_toolcall_with_raw_items(call_id, toolcall, &[])
    }

    pub(crate) fn abort_pending_and_observe_completed_toolcall_with_raw_items(
        &mut self,
        call_id: &str,
        toolcall: CompletedToolCall,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<bool, SpineError> {
        self.ensure_jit_enabled("Spine pending toolcall abort")?;
        if self
            .pending
            .as_ref()
            .is_none_or(|pending| pending.call_id() != call_id)
        {
            return Ok(false);
        }
        let (event, segments) = self.completed_toolcall_parts(&toolcall)?;
        let mut staged_parse_stack = self.parse_stack.clone();
        staged_parse_stack.shift(SpineToken::ToolCall { segments }, &self.archive())?;
        let toolcall_seq = self.append_cached_event(event)?;
        self.parse_stack = staged_parse_stack;
        self.pending = None;
        self.append_trim_candidates_for_completed_toolcall(&toolcall, toolcall_seq, raw_items)?;
        self.clear_completed_toolcall_anchors(&toolcall);
        Ok(true)
    }

    pub(crate) fn commit_completed_toolcall_as_ordinary_with_raw_items(
        &mut self,
        call_id: &str,
        toolcall: CompletedToolCall,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<bool, SpineError> {
        self.ensure_jit_enabled("Spine ordinary toolcall commit")?;
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.call_id() == call_id)
        {
            return self.abort_pending_and_observe_completed_toolcall_with_raw_items(
                call_id, toolcall, raw_items,
            );
        }
        let (event, segments) = self.completed_toolcall_parts(&toolcall)?;
        let mut staged_parse_stack = self.parse_stack.clone();
        staged_parse_stack.shift(SpineToken::ToolCall { segments }, &self.archive())?;
        let toolcall_seq = self.append_cached_event(event)?;
        self.parse_stack = staged_parse_stack;
        self.append_trim_candidates_for_completed_toolcall(&toolcall, toolcall_seq, raw_items)?;
        self.clear_completed_toolcall_anchors(&toolcall);
        Ok(false)
    }

    pub(crate) fn observe_recorded_tool_output_group_as_completed_toolcall(
        &mut self,
        tool_responses: &[(String, u64, usize)],
    ) -> Result<(), SpineError> {
        self.observe_recorded_tool_output_group_as_completed_toolcall_with_raw_items(
            tool_responses,
            &[],
        )
    }

    pub(crate) fn observe_recorded_tool_output_group_as_completed_toolcall_with_raw_items(
        &mut self,
        tool_responses: &[(String, u64, usize)],
        raw_items: &[Option<ResponseItem>],
    ) -> Result<(), SpineError> {
        if !self.jit_enabled {
            return self.observe_recorded_tool_output_group_for_trim(tool_responses, raw_items);
        }
        let mut response_segments = Vec::new();
        let mut request_call_ids = Vec::new();
        for (call_id, raw_ordinal, context_index) in tool_responses {
            if self.control_call_ids.contains(call_id)
                || self
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.call_id() == call_id)
            {
                continue;
            }
            if !request_call_ids.contains(call_id) {
                if !self.ordinary_tool_requests.contains_key(call_id) {
                    continue;
                }
                request_call_ids.push(call_id.clone());
            }
            response_segments.push(CompletedToolCallSegment {
                kind: ToolCallSegmentKind::Response,
                raw_ordinal: *raw_ordinal,
                context_index: *context_index,
            });
        }
        if request_call_ids.is_empty() || response_segments.is_empty() {
            return Ok(());
        }
        request_call_ids.sort_by(|left, right| {
            let left_anchor = self.ordinary_tool_requests.get(left);
            let right_anchor = self.ordinary_tool_requests.get(right);
            left_anchor
                .map(|anchor| (anchor.context_index, anchor.raw_ordinal))
                .cmp(&right_anchor.map(|anchor| (anchor.context_index, anchor.raw_ordinal)))
                .then_with(|| left.cmp(right))
        });
        let mut segments = Vec::with_capacity(request_call_ids.len() + response_segments.len());
        for request_call_id in &request_call_ids {
            let request = self
                .ordinary_tool_requests
                .get(request_call_id)
                .ok_or_else(|| {
                    SpineError::InvalidEvent(format!(
                        "missing tool request anchor for call_id={request_call_id}"
                    ))
                })?;
            segments.push(CompletedToolCallSegment {
                kind: ToolCallSegmentKind::Request,
                raw_ordinal: request.raw_ordinal,
                context_index: usize::try_from(request.context_index).map_err(|_| {
                    SpineError::InvalidEvent("tool request context index overflow".to_string())
                })?,
            });
        }
        response_segments.sort_by_key(|segment| (segment.context_index, segment.raw_ordinal));
        segments.extend(response_segments);
        let call_id = request_call_ids.first().cloned().ok_or_else(|| {
            SpineError::InvalidEvent("completed toolcall missing call id".to_string())
        })?;
        self.observe_completed_toolcall_with_raw_items(
            CompletedToolCall {
                call_id,
                request_call_ids,
                segments,
            },
            raw_items,
        )
    }

    fn observe_recorded_tool_output_group_for_trim(
        &mut self,
        tool_responses: &[(String, u64, usize)],
        raw_items: &[Option<ResponseItem>],
    ) -> Result<(), SpineError> {
        let mut segments = Vec::new();
        let mut request_call_ids = Vec::new();
        for (call_id, raw_ordinal, context_index) in tool_responses {
            if !request_call_ids.contains(call_id) {
                request_call_ids.push(call_id.clone());
            }
            segments.push(CompletedToolCallSegment {
                kind: ToolCallSegmentKind::Response,
                raw_ordinal: *raw_ordinal,
                context_index: *context_index,
            });
        }
        if request_call_ids.is_empty() || segments.is_empty() {
            return Ok(());
        }
        segments.sort_by_key(|segment| (segment.context_index, segment.raw_ordinal));
        let call_id = request_call_ids.first().cloned().ok_or_else(|| {
            SpineError::InvalidEvent("completed trim toolcall missing call id".to_string())
        })?;
        self.observe_completed_toolcall_for_trim(
            CompletedToolCall {
                call_id,
                request_call_ids,
                segments,
            },
            raw_items,
        )
    }

    pub(crate) fn checkpoint_before_user_msg(
        &self,
        rollout_path: &Path,
        raw_ordinal: u64,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<(), SpineError> {
        let raw_end = usize::try_from(raw_ordinal)
            .map_err(|_| SpineError::InvalidEvent("raw ordinal overflow".to_string()))?;
        let prefix = raw_items.get(..raw_end).ok_or_else(|| {
            SpineError::InvalidEvent("checkpoint raw ordinal outside raw history".to_string())
        })?;
        let context = self.materialize_history(prefix)?;
        let checkpoint = build_checkpoint(
            rollout_path,
            raw_ordinal,
            self.ledger.next_event_seq,
            self.pressure_seq_watermark()?,
            self.trim_seq_watermark()?,
            &self.raw_live,
            &self.parse_stack,
            &context,
        )?;
        self.store.write_checkpoint(&checkpoint)
    }

    pub(crate) fn checkpoint_initial(
        &self,
        rollout_path: &Path,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<(), SpineError> {
        let context = self.materialize_history(raw_items)?;
        let mut checkpoint = build_checkpoint(
            rollout_path,
            0,
            self.ledger.next_event_seq,
            self.pressure_seq_watermark()?,
            self.trim_seq_watermark()?,
            &self.raw_live,
            &self.parse_stack,
            &context,
        )?;
        checkpoint.checkpoint_id = "initial".to_string();
        self.store.write_initial_checkpoint(&checkpoint)
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

    fn append_msg_event(&mut self, msg: &PendingMsg) -> Result<u64, SpineError> {
        self.append_cached_event(SpineLedgerEvent::Msg {
            raw_ordinal: msg.raw_ordinal,
            context_index: msg.context_index,
            from_user: msg.from_user,
            user_anchor: msg.user_anchor,
        })
    }

    fn completed_toolcall_parts(
        &self,
        toolcall: &CompletedToolCall,
    ) -> Result<(SpineLedgerEvent, Vec<ToolCallSegment>), SpineError> {
        validate_completed_toolcall(toolcall)?;
        let segments = toolcall
            .segments
            .iter()
            .map(|segment| ToolCallSegment {
                kind: segment.kind,
                seg: SegRef::ResponseItem {
                    raw_ordinal: segment.raw_ordinal,
                    context_index: segment.context_index,
                },
            })
            .collect::<Vec<_>>();
        let event = self.completed_toolcall_event(&segments)?;
        Ok((event, segments))
    }

    fn clear_completed_toolcall_anchors(&mut self, toolcall: &CompletedToolCall) {
        for request_call_id in &toolcall.request_call_ids {
            self.open_requests.remove(request_call_id);
            self.ordinary_tool_requests.remove(request_call_id);
        }
        self.open_requests.remove(&toolcall.call_id);
        self.ordinary_tool_requests.remove(&toolcall.call_id);
        #[cfg(test)]
        {
            for request_call_id in &toolcall.request_call_ids {
                self.pending_tool_responses.remove(request_call_id);
            }
            self.pending_tool_responses.remove(&toolcall.call_id);
        }
        for request_call_id in &toolcall.request_call_ids {
            self.tree_call_ids.remove(request_call_id);
            self.control_call_ids.remove(request_call_id);
        }
        self.tree_call_ids.remove(&toolcall.call_id);
        self.control_call_ids.remove(&toolcall.call_id);
    }

    fn completed_toolcall_event(
        &self,
        segments: &[ToolCallSegment],
    ) -> Result<SpineLedgerEvent, SpineError> {
        let mut event_segments = Vec::with_capacity(segments.len());
        for segment in segments {
            let SegRef::ResponseItem {
                raw_ordinal,
                context_index,
            } = &segment.seg
            else {
                return Err(SpineError::InvalidEvent(
                    "toolcall segment must reference a raw ResponseItem".to_string(),
                ));
            };
            event_segments.push(ToolCallEventSegment {
                kind: segment.kind,
                raw_ordinal: *raw_ordinal,
                context_index: u64::try_from(*context_index).map_err(|_| {
                    SpineError::InvalidEvent("toolcall context index overflow".to_string())
                })?,
            });
        }
        Ok(SpineLedgerEvent::ToolCall {
            segments: event_segments,
        })
    }

    fn append_trim_candidates_for_completed_toolcall(
        &mut self,
        toolcall: &CompletedToolCall,
        toolcall_seq: u64,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<(), SpineError> {
        self.trimmer()
            .on_tool_call(toolcall, toolcall_seq, raw_items, false)
    }

    fn observe_completed_toolcall_for_trim(
        &mut self,
        toolcall: CompletedToolCall,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<(), SpineError> {
        let toolcall_seq = self.ledger.next_trim_seq;
        self.trimmer()
            .on_tool_call(&toolcall, toolcall_seq, raw_items, true)
    }

    fn current_trim_projection(&self) -> Result<TrimProjection, SpineError> {
        if !self.trim_enabled {
            return Ok(TrimProjection::default());
        }
        trim_projection_from_events(
            &self.ledger.trim_events,
            &self.raw_live,
            self.current_trim_structural_seq(),
            None,
        )
    }

    fn latest_live_completed_toolcall_seq(&self) -> Result<Option<u64>, SpineError> {
        if !self.jit_enabled {
            return self.latest_live_trim_toolcall_seq();
        }
        let raw_mask = RawMask::new(&self.raw_live);
        for event in self.ledger.events.iter().rev() {
            if event.seq >= self.ledger.next_event_seq {
                continue;
            }
            if matches!(event.event, SpineLedgerEvent::ToolCall { .. })
                && event.allowed_by(raw_mask)?
            {
                return Ok(Some(event.seq));
            }
        }
        Ok(None)
    }

    fn latest_live_trim_toolcall_seq(&self) -> Result<Option<u64>, SpineError> {
        let raw_mask = RawMask::new(&self.raw_live);
        for event in self.ledger.trim_events.iter().rev() {
            let TrimEvent::ToolCallBoundary { toolcall_seq, .. } = event.event else {
                continue;
            };
            if event.allowed_by(raw_mask)? {
                return Ok(Some(toolcall_seq));
            }
        }
        Ok(None)
    }

    pub(crate) fn trim_tool_response(
        &mut self,
        trim_id: &str,
    ) -> Result<SpineTrimOutcome, SpineError> {
        let latest = self.latest_live_completed_toolcall_seq()?;
        let current_seq = self.current_trim_structural_seq();
        self.trimmer().snip(trim_id, latest, current_seq)
    }

    pub(crate) fn slice_tool_response_head(
        &mut self,
        trim_id: &str,
        head: usize,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<SpineTrimOutcome, SpineError> {
        self.slice_tool_response(trim_id, TrimSliceSpec::Head { head }, raw_items)
    }

    pub(crate) fn slice_tool_response_tail(
        &mut self,
        trim_id: &str,
        tail: usize,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<SpineTrimOutcome, SpineError> {
        self.slice_tool_response(trim_id, TrimSliceSpec::Tail { tail }, raw_items)
    }

    pub(crate) fn slice_tool_response_anchor(
        &mut self,
        trim_id: &str,
        anchor: &str,
        preceding: usize,
        following: usize,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<SpineTrimOutcome, SpineError> {
        self.slice_tool_response(
            trim_id,
            TrimSliceSpec::Anchor {
                anchor: anchor.to_string(),
                preceding,
                following,
            },
            raw_items,
        )
    }

    fn slice_tool_response(
        &mut self,
        trim_id: &str,
        slice: TrimSliceSpec,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<SpineTrimOutcome, SpineError> {
        let latest = self.latest_live_completed_toolcall_seq()?;
        let current_seq = self.current_trim_structural_seq();
        self.trimmer()
            .slice(trim_id, slice, latest, current_seq, raw_items)
    }

    fn remap_completed_toolcall_context_indices(
        &self,
        mut toolcall: CompletedToolCall,
        toolcall_context_start: usize,
    ) -> Result<CompletedToolCall, SpineError> {
        let mut context_index = toolcall_context_start;
        for segment in &mut toolcall.segments {
            segment.context_index = context_index;
            context_index = context_index.checked_add(1).ok_or_else(|| {
                SpineError::InvalidEvent("toolcall context index overflow".to_string())
            })?;
        }
        Ok(toolcall)
    }

    fn push_msg_token(&mut self, msg: &PendingMsg) -> Result<(), SpineError> {
        self.parse_stack.shift(
            SpineToken::Msg {
                seg: SegRef::ResponseItem {
                    raw_ordinal: msg.raw_ordinal,
                    context_index: usize::try_from(msg.context_index).map_err(|_| {
                        SpineError::InvalidEvent("context index overflow".to_string())
                    })?,
                },
                from_user: msg.from_user,
                user_anchor: msg.user_anchor,
            },
            &self.archive(),
        )
    }

    fn push_completed_toolcall_token(
        &mut self,
        segments: Vec<ToolCallSegment>,
    ) -> Result<(), SpineError> {
        self.parse_stack
            .shift(SpineToken::ToolCall { segments }, &self.archive())
    }

    fn append_and_shift_msg(&mut self, msg: &PendingMsg) -> Result<(), SpineError> {
        self.append_msg_event(msg)?;
        self.push_msg_token(msg)
    }

    pub(crate) fn stage_open(
        &mut self,
        call_id: String,
        summary: String,
    ) -> Result<(), SpineError> {
        self.ensure_no_pending_transition()?;
        let summary = summary.trim().to_string();
        if summary.is_empty() {
            return Err(SpineError::ToolUse(
                "spine.open summary must not be empty".to_string(),
            ));
        }
        let anchor = self.open_requests.remove(&call_id).ok_or_else(|| {
            SpineError::Operation(format!(
                "missing spine.open request anchor for call_id={call_id}"
            ))
        })?;
        self.stage(PendingTransition::Open {
            call_id,
            summary,
            boundary: anchor.raw_ordinal,
            index: anchor.context_index,
        })
    }

    pub(crate) fn stage_close<M: IntoSpineNodeMemory>(
        &mut self,
        call_id: String,
        memory: M,
    ) -> Result<(), SpineError> {
        self.ensure_no_pending_transition()?;
        let memory = memory.into_spine_node_memory()?;
        self.validate_memory_user_anchor_refs(&memory)?;
        if !self.control_call_ids.contains(&call_id) {
            return Err(SpineError::Operation(format!(
                "missing spine.close request anchor for call_id={call_id}"
            )));
        }
        self.current_close_open_meta()?;
        self.stage(PendingTransition::Close { call_id, memory })
    }

    pub(crate) fn stage_next<M: IntoSpineNodeMemory>(
        &mut self,
        call_id: String,
        summary: String,
        memory: M,
    ) -> Result<(), SpineError> {
        self.ensure_no_pending_transition()?;
        let summary = summary.trim().to_string();
        if summary.is_empty() {
            return Err(SpineError::ToolUse(
                "spine.next summary must not be empty".to_string(),
            ));
        }
        let memory = memory.into_spine_node_memory()?;
        self.validate_memory_user_anchor_refs(&memory)?;
        if !self.control_call_ids.contains(&call_id) {
            return Err(SpineError::Operation(format!(
                "missing spine.next request anchor for call_id={call_id}"
            )));
        }
        self.current_close_open_meta()?;
        self.stage(PendingTransition::NextSugar {
            call_id,
            summary,
            memory,
        })
    }

    fn validate_memory_user_anchor_refs(&self, memory: &str) -> Result<(), SpineError> {
        let refs = user_anchor_refs_in_memory(memory)?;
        if refs.is_empty() {
            return Ok(());
        }
        let existing = self.live_user_anchors()?;
        for anchor in refs {
            if !existing.contains(&anchor) {
                return Err(SpineError::ToolUse(format!(
                    "spine.close/next memory references unknown user anchor [U{anchor}]"
                )));
            }
        }
        Ok(())
    }

    fn live_user_anchors(&self) -> Result<BTreeSet<u64>, SpineError> {
        let raw_mask = RawMask::new(&self.raw_live);
        let mut anchors = BTreeSet::new();
        for event in &self.ledger.events {
            if !event.allowed_by(raw_mask)? {
                continue;
            }
            if let SpineLedgerEvent::Msg {
                user_anchor: Some(anchor),
                ..
            } = &event.event
            {
                anchors.insert(*anchor);
            }
        }
        Ok(anchors)
    }

    fn stage(&mut self, pending: PendingTransition) -> Result<(), SpineError> {
        self.ensure_no_pending_transition()?;
        self.pending = Some(pending);
        Ok(())
    }

    fn ensure_no_pending_transition(&self) -> Result<(), SpineError> {
        if self.pending.is_some() {
            let pending_call_id = self
                .pending
                .as_ref()
                .map(PendingTransition::call_id)
                .unwrap_or("<unknown>");
            return Err(SpineError::Operation(format!(
                "another spine transition is already pending: call_id={pending_call_id}"
            )));
        }
        Ok(())
    }

    pub(crate) fn abort_pending(&mut self, call_id: &str) -> bool {
        if self
            .pending
            .as_ref()
            .is_none_or(|pending| pending.call_id() != call_id)
        {
            return false;
        }
        let Some(pending) = self.pending.take() else {
            return false;
        };
        self.control_call_ids.remove(pending.call_id());
        true
    }

    pub(crate) fn abort_any_pending(&mut self) -> Option<String> {
        let pending = self.pending.take()?;
        let call_id = pending.call_id().to_string();
        self.control_call_ids.remove(&call_id);
        Some(call_id)
    }

    #[cfg(test)]
    pub(crate) fn maybe_commit_output(
        &mut self,
        call_id: &str,
        memory_assembly: Option<SpineCloseMemoryAssembly>,
    ) -> Result<Option<SpineCommitKind>, SpineError> {
        let completed_toolcall = self.observed_completed_toolcall(call_id)?;
        self.maybe_commit_output_impl(
            call_id,
            memory_assembly,
            SpineTokenBaselines::default(),
            completed_toolcall,
            &[],
        )
        .and_then(|prepared| self.install_prepared_commit_for_kind(prepared))
    }

    #[cfg(test)]
    pub(crate) fn maybe_commit_output_with_open_input_tokens(
        &mut self,
        call_id: &str,
        memory_assembly: Option<SpineCloseMemoryAssembly>,
        input_tokens: Option<i64>,
    ) -> Result<Option<SpineCommitKind>, SpineError> {
        let completed_toolcall = self.observed_completed_toolcall(call_id)?;
        self.maybe_commit_output_impl(
            call_id,
            memory_assembly,
            SpineTokenBaselines {
                provider_input_tokens: input_tokens,
            },
            completed_toolcall,
            &[],
        )
        .and_then(|prepared| self.install_prepared_commit_for_kind(prepared))
    }

    #[cfg(test)]
    pub(crate) fn maybe_commit_output_with_token_baselines(
        &mut self,
        call_id: &str,
        memory_assembly: Option<SpineCloseMemoryAssembly>,
        token_baselines: SpineTokenBaselines,
    ) -> Result<Option<SpineCommitKind>, SpineError> {
        let completed_toolcall = self.observed_completed_toolcall(call_id)?;
        self.maybe_commit_output_impl(
            call_id,
            memory_assembly,
            token_baselines,
            completed_toolcall,
            &[],
        )
        .and_then(|prepared| self.install_prepared_commit_for_kind(prepared))
    }

    pub(crate) fn maybe_commit_output_with_toolcall(
        &mut self,
        call_id: &str,
        memory_assembly: Option<SpineCloseMemoryAssembly>,
        token_baselines: SpineTokenBaselines,
        completed_toolcall: CompletedToolCall,
    ) -> Result<Option<SpineCommitKind>, SpineError> {
        self.maybe_commit_output_with_toolcall_and_raw_items(
            call_id,
            memory_assembly,
            token_baselines,
            completed_toolcall,
            &[],
        )
    }

    pub(crate) fn maybe_commit_output_with_toolcall_and_raw_items(
        &mut self,
        call_id: &str,
        memory_assembly: Option<SpineCloseMemoryAssembly>,
        token_baselines: SpineTokenBaselines,
        completed_toolcall: CompletedToolCall,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<Option<SpineCommitKind>, SpineError> {
        let Some(prepared) = self.prepare_commit_output_with_toolcall_and_raw_items(
            call_id,
            memory_assembly,
            token_baselines,
            completed_toolcall,
            raw_items,
        )?
        else {
            return Ok(None);
        };
        let kind = prepared.kind.clone();
        self.persist_prepared_commit_side_effects(&prepared)?;
        self.install_prepared_commit(prepared);
        Ok(Some(kind))
    }

    pub(crate) fn prepare_commit_output_with_toolcall_and_raw_items(
        &mut self,
        call_id: &str,
        memory_assembly: Option<SpineCloseMemoryAssembly>,
        token_baselines: SpineTokenBaselines,
        completed_toolcall: CompletedToolCall,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<Option<SpinePreparedCommit>, SpineError> {
        self.maybe_commit_output_impl(
            call_id,
            memory_assembly,
            token_baselines,
            Some(completed_toolcall),
            raw_items,
        )
    }

    fn maybe_commit_output_impl(
        &mut self,
        call_id: &str,
        memory_assembly: Option<SpineCloseMemoryAssembly>,
        token_baselines: SpineTokenBaselines,
        completed_toolcall: Option<CompletedToolCall>,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<Option<SpinePreparedCommit>, SpineError> {
        let Some(pending) = self.pending.as_ref() else {
            return Ok(None);
        };
        if pending.call_id() != call_id {
            return Ok(None);
        }
        let pending = pending.clone();
        let commit_kind = match pending {
            PendingTransition::Open {
                summary,
                boundary,
                index,
                ..
            } => self.commit_open_pending(
                summary,
                boundary,
                index,
                token_baselines,
                completed_toolcall,
                raw_items,
            )?,
            PendingTransition::Close { .. } => self.commit_close_pending(
                memory_assembly,
                token_baselines,
                completed_toolcall,
                raw_items,
            )?,
            PendingTransition::NextSugar { summary, .. } => self.commit_next_sugar_pending(
                summary,
                memory_assembly,
                token_baselines,
                completed_toolcall,
                raw_items,
            )?,
        };
        self.pending = None;
        self.control_call_ids.remove(call_id);
        Ok(Some(commit_kind))
    }

    fn install_prepared_commit_for_kind(
        &mut self,
        prepared: Option<SpinePreparedCommit>,
    ) -> Result<Option<SpineCommitKind>, SpineError> {
        let Some(prepared) = prepared else {
            return Ok(None);
        };
        let kind = prepared.kind.clone();
        self.persist_prepared_commit_side_effects(&prepared)?;
        self.install_prepared_commit(prepared);
        Ok(Some(kind))
    }

    fn task_tree_reduced_from(
        &self,
        parse_stack: ParseStack,
        reduction: PreparedTaskTreeReduction,
    ) -> Result<ParseStack, SpineError> {
        parse_stack.task_tree_reduced(reduction)
    }

    fn commit_open_pending(
        &mut self,
        summary: String,
        mut boundary: u64,
        mut index: u64,
        token_baselines: SpineTokenBaselines,
        completed_toolcall: Option<CompletedToolCall>,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<SpinePreparedCommit, SpineError> {
        if let Some(completed_toolcall) = completed_toolcall.as_ref() {
            let first = completed_toolcall_first_segment(completed_toolcall)?;
            boundary = first.raw_ordinal;
            index = u64::try_from(first.context_index).map_err(|_| {
                SpineError::InvalidEvent(
                    "spine.open grouped toolcall context index overflow".to_string(),
                )
            })?;
        }
        let child = self.parse_stack.next_child_id()?;
        let open_context_source = token_baselines
            .provider_input_tokens
            .map(|_| ContextBaselineSource::ProviderAtOpen);
        let event = SpineLedgerEvent::Open {
            child: child.clone(),
            boundary,
            index,
            summary: summary.clone(),
            open_input_tokens: token_baselines.provider_input_tokens,
            open_context_tokens: token_baselines.provider_input_tokens,
            open_context_source,
        };
        let mut staged_parse_stack = self.parse_stack.clone();
        staged_parse_stack.shift(
            SpineToken::Open {
                meta: tree_meta_with_token_baselines(
                    &self.archive(),
                    child,
                    index,
                    summary,
                    token_baselines.provider_input_tokens,
                    open_context_source,
                )?,
            },
            &self.archive(),
        )?;
        if let Some(completed_toolcall) = completed_toolcall {
            let (toolcall_event, segments) = self.completed_toolcall_parts(&completed_toolcall)?;
            staged_parse_stack.shift(SpineToken::ToolCall { segments }, &self.archive())?;
            let toolcall_seq = self.ledger.next_event_seq.checked_add(1).ok_or_else(|| {
                SpineError::InvalidEvent("spine.open toolcall seq overflow".to_string())
            })?;
            let events = vec![event, toolcall_event];
            self.append_committed_events_no_marker(events)?;
            self.parse_stack = staged_parse_stack;
            self.append_trim_candidates_for_completed_toolcall(
                &completed_toolcall,
                toolcall_seq,
                raw_items,
            )?;
            self.clear_completed_toolcall_anchors(&completed_toolcall);
            return Ok(SpinePreparedCommit {
                kind: SpineCommitKind::Open {
                    open_request_index: usize::try_from(index).map_err(|_| {
                        SpineError::InvalidEvent("spine.open context index overflow".to_string())
                    })?,
                },
                publication_plan: None,
                final_parse_stack: None,
                completed_toolcall: None,
                toolcall_seq: None,
                raw_items: Vec::new(),
                mem_for_accounting: None,
            });
        }
        let events = vec![event];
        self.append_committed_events_no_marker(events)?;
        self.parse_stack = staged_parse_stack;
        Ok(SpinePreparedCommit {
            kind: SpineCommitKind::Open {
                open_request_index: usize::try_from(index).map_err(|_| {
                    SpineError::InvalidEvent("spine.open context index overflow".to_string())
                })?,
            },
            publication_plan: None,
            final_parse_stack: None,
            completed_toolcall: None,
            toolcall_seq: None,
            raw_items: Vec::new(),
            mem_for_accounting: None,
        })
    }

    fn commit_close_pending(
        &mut self,
        memory_assembly: Option<SpineCloseMemoryAssembly>,
        token_baselines: SpineTokenBaselines,
        completed_toolcall: Option<CompletedToolCall>,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<SpinePreparedCommit, SpineError> {
        self.commit_close_family_pending(
            CloseFamilyAfterClose::None,
            memory_assembly,
            token_baselines,
            completed_toolcall,
            raw_items,
        )
    }

    fn commit_next_sugar_pending(
        &mut self,
        summary: String,
        memory_assembly: Option<SpineCloseMemoryAssembly>,
        token_baselines: SpineTokenBaselines,
        completed_toolcall: Option<CompletedToolCall>,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<SpinePreparedCommit, SpineError> {
        self.commit_close_family_pending(
            CloseFamilyAfterClose::Open { summary },
            memory_assembly,
            token_baselines,
            completed_toolcall,
            raw_items,
        )
    }

    fn commit_close_family_pending(
        &mut self,
        after_close: CloseFamilyAfterClose,
        memory_assembly: Option<SpineCloseMemoryAssembly>,
        token_baselines: SpineTokenBaselines,
        completed_toolcall: Option<CompletedToolCall>,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<SpinePreparedCommit, SpineError> {
        let prepared = self.prepare_close_commit(memory_assembly, token_baselines)?;
        let plan = self.close_family_plan(&prepared, after_close)?;
        let mut events = vec![prepared.close_event.clone()];
        if let Some(open) = plan.open.as_ref() {
            events.push(open.event.clone());
        }
        let completed_toolcall = completed_toolcall
            .ok_or_else(|| SpineError::InvalidEvent(plan.missing_toolcall_error.to_string()))?;
        let toolcall_start = completed_toolcall_first_segment(&completed_toolcall)?.context_index;
        let toolcall_context_index = match plan.toolcall_context_index {
            Some(index) => index,
            None => prepared
                .suffix_start
                .checked_add(prepared.replacement.len())
                .ok_or_else(|| {
                    SpineError::InvalidEvent(
                        "spine.close toolcall context index overflow".to_string(),
                    )
                })?,
        };
        let completed_toolcall = self
            .remap_completed_toolcall_context_indices(completed_toolcall, toolcall_context_index)?;
        let (toolcall_event, segments) = self.completed_toolcall_parts(&completed_toolcall)?;
        events.push(toolcall_event);
        let event_count = u64::try_from(events.len())
            .map_err(|_| SpineError::InvalidEvent("spine event count overflow".to_string()))?;
        let toolcall_seq = self
            .ledger
            .next_event_seq
            .checked_add(event_count.checked_sub(1).ok_or_else(|| {
                SpineError::InvalidEvent(plan.event_count_underflow_error.to_string())
            })?)
            .ok_or_else(|| {
                SpineError::InvalidEvent(plan.toolcall_seq_overflow_error.to_string())
            })?;
        let mut pending_close_parse_stack = self.parse_stack.clone();
        pending_close_parse_stack.shift_pending_close(prepared.memory.clone(), &self.archive())?;
        let mut final_parse_stack = self.task_tree_reduced_from(
            pending_close_parse_stack.clone(),
            prepared.task_tree_reduction,
        )?;
        if let Some(open) = plan.open.as_ref() {
            final_parse_stack.shift(
                SpineToken::Open {
                    meta: tree_meta_with_token_baselines(
                        &self.archive(),
                        open.child.clone(),
                        open.open_index_u64,
                        open.summary.clone(),
                        None,
                        None,
                    )?,
                },
                &self.archive(),
            )?;
        }
        final_parse_stack.shift(SpineToken::ToolCall { segments }, &self.archive())?;
        if let Err(err) = self.commit_close_family_transaction(CloseFamilyTransaction {
            mem: &prepared.mem,
            memory_body: &prepared.memory_body,
            archive_writes: &prepared.archive_writes,
            events,
            marker_kind: plan.marker_kind,
            close_event: &prepared.close_event,
            event_count,
        }) {
            match err {
                CloseFamilyTransactionError::PreparedSideEffect(err) => {
                    self.parse_stack = pending_close_parse_stack;
                    return Err(err);
                }
                CloseFamilyTransactionError::CommitProof(err) => return Err(err),
            }
        }
        Ok(SpinePreparedCommit {
            kind: plan.kind,
            publication_plan: Some(HistoryPublicationPlan {
                operation: plan.operation,
                suffix_start: prepared.suffix_start,
                replacement_prefix: prepared.replacement,
                preserve_host_history_from: toolcall_start,
                append_current_tool_response_if_missing: true,
            }),
            final_parse_stack: Some(final_parse_stack),
            completed_toolcall: Some(completed_toolcall),
            toolcall_seq: Some(toolcall_seq),
            raw_items: raw_items.to_vec(),
            mem_for_accounting: Some(prepared.mem),
        })
    }

    fn close_family_plan(
        &self,
        prepared: &PreparedCloseCommit,
        after_close: CloseFamilyAfterClose,
    ) -> Result<CloseFamilyPlan, SpineError> {
        match after_close {
            CloseFamilyAfterClose::None => Ok(CloseFamilyPlan {
                operation: "spine.close",
                missing_toolcall_error: "spine.close commit requires completed toolcall evidence",
                event_count_underflow_error: "spine close event count underflow",
                toolcall_seq_overflow_error: "spine.close toolcall seq overflow",
                marker_kind: SpineCommitKindMarker::Close,
                kind: SpineCommitKind::Close,
                toolcall_context_index: None,
                open: None,
            }),
            CloseFamilyAfterClose::Open { summary } => {
                let mut close_reduced_parse_stack = self.parse_stack.clone();
                close_reduced_parse_stack
                    .shift_pending_close(prepared.memory.clone(), &self.archive())?;
                close_reduced_parse_stack
                    .apply_prevalidated_task_tree_reduction(prepared.task_tree_reduction.clone());
                let child = close_reduced_parse_stack.next_child_id()?;
                let open_index = prepared
                    .suffix_start
                    .checked_add(prepared.replacement.len())
                    .ok_or_else(|| {
                        SpineError::InvalidEvent(
                            "spine.next synthetic open index overflow".to_string(),
                        )
                    })?;
                let open_index_u64 = u64::try_from(open_index).map_err(|_| {
                    SpineError::InvalidEvent("spine.next synthetic open index overflow".to_string())
                })?;
                let event = SpineLedgerEvent::Open {
                    child: child.clone(),
                    boundary: self.raw_len,
                    index: open_index_u64,
                    summary: summary.clone(),
                    open_input_tokens: None,
                    open_context_tokens: None,
                    open_context_source: None,
                };
                Ok(CloseFamilyPlan {
                    operation: "spine.next",
                    missing_toolcall_error: "spine.next commit requires completed toolcall evidence",
                    event_count_underflow_error: "spine next event count underflow",
                    toolcall_seq_overflow_error: "spine.next toolcall seq overflow",
                    marker_kind: SpineCommitKindMarker::CloseThenOpen,
                    kind: SpineCommitKind::CloseThenOpen { open_index },
                    toolcall_context_index: Some(open_index),
                    open: Some(CloseFamilyOpenPlan {
                        child,
                        open_index_u64,
                        summary,
                        event,
                    }),
                })
            }
        }
    }

    fn commit_close_family_transaction(
        &mut self,
        tx: CloseFamilyTransaction<'_>,
    ) -> Result<(), CloseFamilyTransactionError> {
        self.write_prepared_memory_body(tx.mem, tx.memory_body)
            .and_then(|()| flush_archive_writes(tx.archive_writes))
            .and_then(|()| self.commit_prepared_memory_record(tx.mem, tx.memory_body))
            .map_err(CloseFamilyTransactionError::PreparedSideEffect)?;
        let marker = close_commit_marker(
            self.ledger.next_event_seq,
            tx.mem,
            tx.marker_kind,
            close_event_boundary(tx.close_event)
                .map_err(CloseFamilyTransactionError::CommitProof)?,
            tx.event_count,
        )
        .map_err(CloseFamilyTransactionError::CommitProof)?;
        self.append_committed_events(tx.events, marker)
            .map_err(CloseFamilyTransactionError::CommitProof)?;
        Ok(())
    }

    pub(crate) fn persist_prepared_commit_side_effects(
        &mut self,
        prepared: &SpinePreparedCommit,
    ) -> Result<(), SpineError> {
        if let (Some(completed_toolcall), Some(toolcall_seq)) =
            (prepared.completed_toolcall.as_ref(), prepared.toolcall_seq)
        {
            self.append_trim_candidates_for_completed_toolcall(
                completed_toolcall,
                toolcall_seq,
                &prepared.raw_items,
            )?;
        }
        if let Some(mem) = prepared.mem_for_accounting.as_ref() {
            self.register_pending_memory_context_accounting(mem)?;
        }
        Ok(())
    }

    pub(crate) fn install_prepared_commit(&mut self, prepared: SpinePreparedCommit) {
        if let Some(final_parse_stack) = prepared.final_parse_stack {
            self.parse_stack = final_parse_stack;
        }
        if let Some(completed_toolcall) = prepared.completed_toolcall.as_ref() {
            self.clear_completed_toolcall_anchors(completed_toolcall);
        }
    }

    fn prepare_close_commit(
        &self,
        memory_assembly: Option<SpineCloseMemoryAssembly>,
        token_baselines: SpineTokenBaselines,
    ) -> Result<PreparedCloseCommit, SpineError> {
        let memory_assembly = memory_assembly.ok_or_else(|| {
            SpineError::CompactFailure(
                "spine.close requires a validated source plan for memory assembly".to_string(),
            )
        })?;
        let open_meta = self.current_close_open_meta()?.clone();
        let node = open_meta.id.clone();
        if !self.parse_stack.current_open_has_nodes()? {
            return Err(SpineError::Operation(format!(
                "spine.close requires non-empty live suffix for node {node}"
            )));
        }
        let suffix_start = open_meta.index;
        let close_event = SpineLedgerEvent::Close {
            node,
            boundary: self.raw_len,
            summary: open_meta.summary.clone(),
            close_input_tokens: token_baselines.provider_input_tokens,
            close_context_tokens: token_baselines.provider_input_tokens,
        };
        let seq = self.ledger.next_event_seq;
        if memory_assembly.source_context_range.start != suffix_start {
            return Err(SpineError::CompactFailure(format!(
                "spine.close memory source context range starts at {}, expected suffix start {suffix_start} for node {}",
                memory_assembly.source_context_range.start, open_meta.id
            )));
        }
        let expected_raw_start = self.open_raw_start(&open_meta.id)?;
        if memory_assembly.source_raw_range.start != expected_raw_start {
            return Err(SpineError::CompactFailure(format!(
                "spine.close memory source raw range starts at {}, expected raw start {expected_raw_start} for node {}",
                memory_assembly.source_raw_range.start, open_meta.id
            )));
        }
        if memory_assembly.source_raw_range.end > self.raw_len {
            return Err(SpineError::CompactFailure(format!(
                "spine.close memory source raw range end {} exceeds raw_len {} for node {}",
                memory_assembly.source_raw_range.end, self.raw_len, open_meta.id
            )));
        }
        let body = memory_assembly.body.clone();
        let mem = self.stage_close_mem(&open_meta, &memory_assembly, token_baselines)?;
        let memory = memory_ref(
            &self.archive(),
            mem.compact_id.clone(),
            mem.node.clone(),
            mem.body_hash.clone(),
            mem.raw_start..mem.raw_end,
            mem.context_start..mem.context_end,
            seq..seq + 1,
            mem.open_input_tokens,
            mem.close_input_tokens,
            mem.open_context_tokens,
            mem.close_context_tokens,
            mem.closed_source_suffix_tokens,
            mem.closed_memory_context_tokens,
            mem.open_context_source,
            mem.memory_output_tokens,
        );
        let staged_archive = SpineArchive::staged_with_memory_body(
            self.store.root.clone(),
            mem.compact_id.clone(),
            body.clone(),
        );
        let task_tree_reduction = self
            .parse_stack
            .prepare_current_task_tree_reduction(&staged_archive, memory.clone())?;
        let archive_writes = staged_archive.staged_writes();
        let replacement = vec![memory_response_item(&body)];
        Ok(PreparedCloseCommit {
            suffix_start,
            replacement,
            mem,
            memory_body: body,
            archive_writes,
            close_event,
            memory,
            task_tree_reduction,
        })
    }

    pub(crate) fn pending_commit(
        &self,
        call_id: &str,
    ) -> Result<Option<SpinePendingCommit>, SpineError> {
        let Some(pending) = self.pending.as_ref() else {
            return Ok(None);
        };
        if pending.call_id() != call_id {
            return Ok(None);
        }
        Ok(Some(match pending {
            PendingTransition::Open { .. } => SpinePendingCommit::Open,
            PendingTransition::Close { memory, .. } => {
                let open_meta = self.current_close_open_meta()?;
                SpinePendingCommit::Close {
                    action: SpinePendingCloseAction::Close,
                    node: open_meta.id.clone(),
                    suffix_start: open_meta.index,
                    memory: memory.clone(),
                    next_summary: None,
                }
            }
            PendingTransition::NextSugar {
                summary, memory, ..
            } => {
                let open_meta = self.current_close_open_meta()?;
                SpinePendingCommit::Close {
                    action: SpinePendingCloseAction::Next,
                    node: open_meta.id.clone(),
                    suffix_start: open_meta.index,
                    memory: memory.clone(),
                    next_summary: Some(summary.clone()),
                }
            }
        }))
    }

    pub(crate) fn pending_tool_request_anchor(
        &self,
        call_id: &str,
    ) -> Result<ToolRequestAnchor, SpineError> {
        if let Some(anchor) = self.open_requests.get(call_id) {
            return Ok(ToolRequestAnchor {
                raw_ordinal: anchor.raw_ordinal,
                context_index: usize::try_from(anchor.context_index).map_err(|_| {
                    SpineError::InvalidEvent("spine.open context index overflow".to_string())
                })?,
            });
        }
        let Some(request) = self.ordinary_tool_requests.get(call_id) else {
            return Err(SpineError::Operation(format!(
                "missing tool request anchor for call_id={call_id}"
            )));
        };
        Ok(ToolRequestAnchor {
            raw_ordinal: request.raw_ordinal,
            context_index: usize::try_from(request.context_index).map_err(|_| {
                SpineError::InvalidEvent("tool request context index overflow".to_string())
            })?,
        })
    }

    #[cfg(test)]
    fn observed_completed_toolcall(
        &self,
        call_id: &str,
    ) -> Result<Option<CompletedToolCall>, SpineError> {
        let Some(responses) = self.pending_tool_responses.get(call_id) else {
            return Ok(None);
        };
        if responses.is_empty() {
            return Ok(None);
        }
        let request = self.pending_tool_request_anchor(call_id)?;
        let mut response_context_indices = Vec::with_capacity(responses.len());
        for response in responses {
            response_context_indices.push(usize::try_from(response.context_index).map_err(
                |_| SpineError::InvalidEvent("tool response context index overflow".to_string()),
            )?);
        }
        Ok(Some(CompletedToolCall {
            call_id: call_id.to_string(),
            request_call_ids: vec![call_id.to_string()],
            segments: std::iter::once(CompletedToolCallSegment {
                kind: ToolCallSegmentKind::Request,
                raw_ordinal: request.raw_ordinal,
                context_index: request.context_index,
            })
            .chain(responses.iter().zip(response_context_indices).map(
                |(response, context_index)| CompletedToolCallSegment {
                    kind: ToolCallSegmentKind::Response,
                    raw_ordinal: response.raw_ordinal,
                    context_index,
                },
            ))
            .collect(),
        }))
    }

    pub(crate) fn build_close_source_plan(
        &self,
        raw_context_items: &[ResponseItem],
        node: &NodeId,
        suffix_start: usize,
        toolcall_start: usize,
        close_call_id: &str,
    ) -> Result<SpineCompactSourcePlan, SpineError> {
        let open_meta = self.current_close_open_meta()?;
        if &open_meta.id != node {
            return Err(SpineError::Invariant(format!(
                "spine.close source plan requested for node {node}, but current close node is {}",
                open_meta.id
            )));
        }
        if open_meta.index != suffix_start {
            return Err(SpineError::CompactFailure(format!(
                "spine.close source plan suffix start {suffix_start} does not match h(PS) open index {} for node {node}",
                open_meta.index
            )));
        }
        if !self.parse_stack.current_open_has_nodes()? {
            return Err(SpineError::Operation(format!(
                "spine.close requires non-empty live suffix for node {node}"
            )));
        }
        if suffix_start >= raw_context_items.len() {
            return Err(SpineError::Operation(format!(
                "spine.close suffix start {suffix_start} is outside history length {} for node {node}",
                raw_context_items.len()
            )));
        }

        let close_context_end = toolcall_start;
        if close_context_end < suffix_start {
            return Err(SpineError::Operation(format!(
                "spine.close request index {close_context_end} precedes suffix start {suffix_start} for node {node} call_id={close_call_id}"
            )));
        }
        if close_context_end == suffix_start {
            return Err(SpineError::Operation(format!(
                "spine.close requires non-empty live suffix for node {node} call_id={close_call_id}"
            )));
        }

        let suffix_nodes = self.current_open_suffix_nodes()?;
        let visible_refs = project_spine_tree_nodes_visible_items(suffix_nodes, suffix_start)?;
        let projected_context_end =
            suffix_start
                .checked_add(visible_refs.len())
                .ok_or_else(|| {
                    SpineError::InvalidEvent(
                        "spine.close source plan context range overflow".to_string(),
                    )
                })?;
        if projected_context_end != close_context_end {
            return Err(SpineError::CompactFailure(format!(
                "spine.close h(PS) suffix projects to [{suffix_start}..{projected_context_end}) but source context range is [{suffix_start}..{close_context_end}) for node {node} call_id={close_call_id}"
            )));
        }
        let entries =
            collect_source_plan_entries_from_visible_refs(&visible_refs, raw_context_items)?;

        if entries.is_empty() {
            return Err(SpineError::Operation(format!(
                "spine.close requires non-empty live suffix for node {}",
                open_meta.id
            )));
        }

        let mut previous_context_index = None;
        for (expected_ordinal, entry) in entries.iter().enumerate() {
            if entry.source_ordinal != expected_ordinal {
                return Err(SpineError::Invariant(format!(
                    "spine.close source plan ordinal {} is not contiguous at expected ordinal {expected_ordinal}",
                    entry.source_ordinal
                )));
            }
            validate_source_plan_context_index(
                entry.source_ordinal,
                entry.context_index,
                suffix_start,
                close_context_end,
                &mut previous_context_index,
            )?;
            let host_item = raw_context_items.get(entry.context_index).ok_or_else(|| {
                SpineError::CompactFailure(format!(
                    "spine.close source plan entry ordinal {} context_index {} exceeds host history length {}",
                    entry.source_ordinal,
                    entry.context_index,
                    raw_context_items.len()
                ))
            })?;
            let expected_item = entry.visible_response_item();
            let host_hash = hash_response_items(std::slice::from_ref(host_item))?;
            if host_item != &expected_item || host_hash != entry.source_hash {
                return Err(SpineError::CompactFailure(format!(
                    "spine.close source plan mismatch at ordinal {} context_index {} source_hash {} host_hash {host_hash}",
                    entry.source_ordinal, entry.context_index, entry.source_hash
                )));
            }
        }

        let source_raw_start = self.open_raw_start(&open_meta.id)?;
        let source_raw_end =
            entries
                .iter()
                .try_fold(source_raw_start, |end, entry| -> Result<u64, SpineError> {
                    Ok(match &entry.kind {
                        SpineCompactSourceEntryKind::RawResponseItem { raw_ordinal, .. } => end
                            .max(raw_ordinal.checked_add(1).ok_or_else(|| {
                                SpineError::InvalidEvent(
                                    "spine.close source plan raw ordinal overflow".to_string(),
                                )
                            })?),
                        SpineCompactSourceEntryKind::ChildMemory {
                            source_raw_range, ..
                        } => end.max(source_raw_range.end),
                    })
                })?;

        Ok(SpineCompactSourcePlan {
            node_id: open_meta.id.clone(),
            source_context_range: suffix_start..close_context_end,
            source_raw_range: source_raw_start..source_raw_end,
            entries,
        })
    }

    fn current_open_suffix_nodes(&self) -> Result<&[SpineTreeNode], SpineError> {
        let open_idx = self
            .parse_stack
            .symbols
            .iter()
            .rposition(|symbol| matches!(symbol, Symbol::Control(ControlSymbol::Open(_))))
            .ok_or_else(|| SpineError::InvalidEvent("ParseStack has no live Open".to_string()))?;
        let suffix = &self.parse_stack.symbols[open_idx + 1..];
        match suffix {
            [Symbol::SpineTreeNodes(nodes)]
            | [
                Symbol::SpineTreeNodes(nodes),
                Symbol::Control(ControlSymbol::Close(_)),
            ] => Ok(nodes),
            _ => Err(SpineError::InvalidEvent(format!(
                "spine.close source plan expected live node list after current Open, found {suffix:?}"
            ))),
        }
    }

    pub(crate) fn is_control_output_call_id(&self, call_id: &str) -> bool {
        self.control_call_ids.contains(call_id)
            || self
                .pending
                .as_ref()
                .is_some_and(|pending| pending.call_id() == call_id)
    }

    #[cfg(test)]
    pub(crate) fn root_compact(
        &mut self,
        body: String,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<Vec<ResponseItem>, SpineError> {
        let prepared = self.root_compact_impl(
            body,
            raw_items,
            SpineRootCompactTokenMetadata::default(),
            None,
        )?;
        let result = prepared.result.clone();
        self.install_prepared_root_compact(prepared);
        Ok(result.materialized)
    }

    pub(crate) fn root_compact_with_checkpoint(
        &mut self,
        rollout_path: &Path,
        body: String,
        raw_items: &[Option<ResponseItem>],
        token_metadata: SpineRootCompactTokenMetadata,
    ) -> Result<SpineRootCompactResult, SpineError> {
        let prepared = self.prepare_root_compact_with_checkpoint(
            rollout_path,
            body,
            raw_items,
            token_metadata,
        )?;
        let result = prepared.result.clone();
        self.install_prepared_root_compact(prepared);
        Ok(result)
    }

    pub(crate) fn prepare_root_compact_with_checkpoint(
        &mut self,
        rollout_path: &Path,
        body: String,
        raw_items: &[Option<ResponseItem>],
        token_metadata: SpineRootCompactTokenMetadata,
    ) -> Result<SpinePreparedRootCompact, SpineError> {
        self.root_compact_impl(body, raw_items, token_metadata, Some(rollout_path))
    }

    fn root_compact_impl(
        &mut self,
        body: String,
        raw_items: &[Option<ResponseItem>],
        token_metadata: SpineRootCompactTokenMetadata,
        checkpoint_rollout_path: Option<&Path>,
    ) -> Result<SpinePreparedRootCompact, SpineError> {
        let token_metadata = SpineRootCompactTokenMetadata {
            next_open_input_tokens: None,
            next_open_context_tokens: None,
            ..token_metadata
        };
        let prepared = self.prepare_root_compact_commit(
            body,
            raw_items,
            token_metadata,
            checkpoint_rollout_path,
        )?;
        let mut pending_compact_parse_stack = self.parse_stack.clone();
        pending_compact_parse_stack.shift_pending_compact(
            prepared.memory.clone(),
            prepared.next_open_index,
            token_metadata.next_open_input_tokens,
            token_metadata.next_open_context_tokens,
            &self.archive(),
        )?;
        let final_parse_stack = self.root_epoch_reduced_from(
            pending_compact_parse_stack.clone(),
            prepared.root_epoch_reduction,
        )?;
        if let Err(err) = self
            .write_prepared_memory_body(&prepared.mem, &prepared.memory_body)
            .and_then(|()| self.commit_prepared_memory_record(&prepared.mem, &prepared.memory_body))
            .and_then(|()| {
                if let Some(checkpoint) = prepared.compact_checkpoint.as_ref() {
                    self.store.append_compact_checkpoint(checkpoint)?;
                }
                Ok(())
            })
        {
            self.parse_stack = pending_compact_parse_stack;
            return Err(err);
        }
        let marker = root_compact_commit_marker(self.ledger.next_event_seq, &prepared.mem)?;
        self.append_committed_events(vec![prepared.root_compact_event], marker)?;
        self.pending = None;
        Ok(SpinePreparedRootCompact {
            result: prepared.result,
            final_parse_stack,
        })
    }

    fn root_epoch_reduced_from(
        &self,
        parse_stack: ParseStack,
        reduction: PreparedRootEpochReduction,
    ) -> Result<ParseStack, SpineError> {
        parse_stack.root_epoch_reduced(reduction)
    }

    pub(crate) fn install_prepared_root_compact(&mut self, prepared: SpinePreparedRootCompact) {
        self.parse_stack = prepared.final_parse_stack;
    }

    fn write_prepared_memory_body(&self, mem: &MemRecord, body: &str) -> Result<(), SpineError> {
        self.store.write_memory_body(&mem.compact_id, body)?;
        Ok(())
    }

    fn commit_prepared_memory_record(&self, mem: &MemRecord, body: &str) -> Result<(), SpineError> {
        let existing_mems = self.store.mems()?;
        let matching_mems = existing_mems
            .iter()
            .filter(|existing| existing.compact_id == mem.compact_id)
            .collect::<Vec<_>>();
        match matching_mems.as_slice() {
            [] => self.store.append_mem(mem),
            [existing] if mem_record_matches(existing, mem) => {
                let existing_body = self.store.read_memory_body(existing)?;
                let existing_body_hash = sha1_hex(existing_body.as_bytes());
                let body_hash = sha1_hex(body.as_bytes());
                if existing_body_hash != body_hash || existing_body_hash != mem.body_hash {
                    return Err(SpineError::InvalidStore(format!(
                        "existing prepared memory body mismatch for {}",
                        mem.compact_id
                    )));
                }
                Ok(())
            }
            [_] => Err(SpineError::InvalidStore(format!(
                "existing prepared memory record mismatch for {}",
                mem.compact_id
            ))),
            _ => Err(SpineError::InvalidStore(format!(
                "ambiguous existing prepared memory record for {}",
                mem.compact_id
            ))),
        }
    }

    fn prepare_root_compact_commit(
        &self,
        body: String,
        raw_items: &[Option<ResponseItem>],
        token_metadata: SpineRootCompactTokenMetadata,
        checkpoint_rollout_path: Option<&Path>,
    ) -> Result<PreparedRootCompactCommit, SpineError> {
        if body.trim().is_empty() {
            return Err(SpineError::CompactFailure(
                "spine root compact memory body must not be empty".to_string(),
            ));
        }
        let source_context_end = self.materialize_history(raw_items)?.len();
        let node = self.parse_stack.current_root_epoch_id()?;
        let compact_id = format!("root-{}-{}", node.as_path().replace('.', "-"), self.raw_len);
        let raw_live_hash = hash_raw_live(&self.raw_live);
        let body_hash = sha1_hex(body.as_bytes());
        let mem = MemRecord {
            compact_id: compact_id.clone(),
            kind: MemKind::RootEpoch,
            node: node.clone(),
            raw_start: 0,
            raw_end: self.raw_len,
            context_start: 0,
            context_end: source_context_end,
            raw_live_hash: Some(raw_live_hash.clone()),
            open_input_tokens: None,
            close_input_tokens: token_metadata.close_input_tokens,
            open_context_tokens: None,
            close_context_tokens: token_metadata.close_context_tokens,
            closed_source_suffix_tokens: None,
            closed_memory_context_tokens: None,
            open_context_source: None,
            memory_output_tokens: None,
            body_path: format!("{BODY_DIR}/{compact_id}.md"),
            body_hash,
        };
        let seq = self.ledger.next_event_seq;
        let memory = memory_ref(
            &self.archive(),
            mem.compact_id.clone(),
            mem.node.clone(),
            mem.body_hash.clone(),
            mem.raw_start..mem.raw_end,
            mem.context_start..mem.context_end,
            seq..seq + 1,
            mem.open_input_tokens,
            mem.close_input_tokens,
            mem.open_context_tokens,
            mem.close_context_tokens,
            mem.closed_source_suffix_tokens,
            mem.closed_memory_context_tokens,
            mem.open_context_source,
            mem.memory_output_tokens,
        );

        let staged_memory_body = Some((compact_id.as_str(), body.as_str()));
        let trim_projection = self.current_trim_projection()?;
        let next_open_index_usize = match self.parse_stack.pending_compact_next_open_index(
            &memory,
            token_metadata.next_open_input_tokens,
            token_metadata.next_open_context_tokens,
        )? {
            Some(next_open_index) => next_open_index,
            None => {
                // Probe first because source_context_range records the pre-compact source
                // span, while next_open_index is the post-compact h(PS) materialized len.
                let mut probe_parse_stack = self.parse_stack.clone();
                probe_parse_stack.shift(
                    SpineToken::Compact {
                        memory: memory.clone(),
                        next_open_index: 0,
                        next_open_input_tokens: token_metadata.next_open_input_tokens,
                        next_open_context_tokens: token_metadata.next_open_context_tokens,
                    },
                    &self.archive(),
                )?;
                render_parse_stack_to_context_with_memory_body_and_trim_projection(
                    &probe_parse_stack,
                    raw_items,
                    staged_memory_body,
                    &trim_projection,
                )?
                .len()
            }
        };

        let mut staged_parse_stack = self.parse_stack.clone();
        staged_parse_stack.shift_pending_compact(
            memory.clone(),
            next_open_index_usize,
            token_metadata.next_open_input_tokens,
            token_metadata.next_open_context_tokens,
            &self.archive(),
        )?;
        let root_epoch_reduction = staged_parse_stack.prepare_root_epoch_reduction(
            &self.archive(),
            memory.clone(),
            next_open_index_usize,
            token_metadata.next_open_input_tokens,
            token_metadata.next_open_context_tokens,
        )?;
        staged_parse_stack.apply_prevalidated_root_epoch_reduction(root_epoch_reduction.clone());
        let materialized = render_parse_stack_to_context_with_memory_body_and_trim_projection(
            &staged_parse_stack,
            raw_items,
            staged_memory_body,
            &trim_projection,
        )?;
        let current_open_index = staged_parse_stack.current_open_meta()?.index;
        if current_open_index != materialized.len() {
            return Err(SpineError::Invariant(format!(
                "spine root compact open index {current_open_index} does not match materialized history length {}",
                materialized.len()
            )));
        }
        let next_open_index_u64 = u64::try_from(next_open_index_usize)
            .map_err(|_| SpineError::InvalidEvent("root open index overflow".to_string()))?;
        let token_seq_after = seq.checked_add(1).ok_or_else(|| {
            SpineError::InvalidEvent("root compact token seq overflow".to_string())
        })?;
        let result = SpineRootCompactResult {
            materialized,
            raw_boundary: self.raw_len,
            token_seq_after,
        };
        let compact_checkpoint = checkpoint_rollout_path
            .map(|rollout_path| {
                build_compact_checkpoint(
                    rollout_path,
                    result.raw_boundary,
                    result.token_seq_after,
                    &self.raw_live,
                    raw_items,
                    &staged_parse_stack,
                    &result.materialized,
                    &result.materialized,
                )
            })
            .transpose()?;
        let root_compact_event = SpineLedgerEvent::RootCompact {
            node,
            boundary: self.raw_len,
            mem: compact_id,
            next_open_index: next_open_index_u64,
            raw_live_hash,
            next_open_input_tokens: token_metadata.next_open_input_tokens,
            next_open_context_tokens: token_metadata.next_open_context_tokens,
        };
        Ok(PreparedRootCompactCommit {
            result,
            mem,
            memory_body: body,
            compact_checkpoint,
            root_compact_event,
            memory,
            root_epoch_reduction,
            next_open_index: next_open_index_usize,
        })
    }

    fn stage_close_mem(
        &self,
        open_meta: &TreeMeta,
        memory_assembly: &SpineCloseMemoryAssembly,
        token_baselines: SpineTokenBaselines,
    ) -> Result<MemRecord, SpineError> {
        let node_id = open_meta.id.clone();
        let raw_start = memory_assembly.source_raw_range.start;
        let end = memory_assembly.source_raw_range.end;
        let compact_id = format!(
            "mem-{}-{}-{}",
            node_id.as_path().replace('.', "-"),
            raw_start,
            end
        );
        let body_path = format!("{BODY_DIR}/{compact_id}.md");
        let open_context_baseline =
            self.open_context_baseline_for(open_meta)
                .map_err(|problem| {
                    SpineError::InvalidEvent(format!(
                        "corrupt provider input baseline for node {}: {problem:?}",
                        open_meta.id
                    ))
                })?;
        let open_input_tokens = open_meta.open_input_tokens;
        let open_context_tokens =
            open_context_baseline.map(|baseline| baseline.provider_input_tokens);
        let closed_source_suffix_tokens = open_context_baseline
            .map(|baseline| baseline.provider_input_tokens)
            .zip(token_baselines.provider_input_tokens)
            .and_then(|(open, close)| (close >= open).then_some(close - open));
        let mem = MemRecord {
            compact_id,
            kind: MemKind::Suffix,
            node: node_id,
            raw_start,
            raw_end: end,
            context_start: memory_assembly.source_context_range.start,
            context_end: memory_assembly.source_context_range.end,
            raw_live_hash: None,
            open_input_tokens,
            close_input_tokens: token_baselines.provider_input_tokens,
            open_context_tokens,
            close_context_tokens: token_baselines.provider_input_tokens,
            closed_source_suffix_tokens,
            closed_memory_context_tokens: None,
            open_context_source: open_context_baseline.map(|baseline| baseline.source),
            memory_output_tokens: memory_assembly.memory_output_tokens,
            body_path,
            body_hash: sha1_hex(memory_assembly.body.as_bytes()),
        };
        Ok(mem)
    }

    fn register_pending_memory_context_accounting(
        &mut self,
        mem: &MemRecord,
    ) -> Result<(), SpineError> {
        let Some(baseline) = replacement_prefix_baseline_tokens(mem) else {
            if let Some(existing) = self.pending_memory_context_accounting.take() {
                self.consume_memory_context_accounting_pending(
                    existing,
                    None,
                    MemoryContextAccountingSkipReason::SupersededByNewPending,
                )?;
            }
            return Ok(());
        };
        self.append_memory_context_accounting_pending(PendingMemoryContextAccounting {
            compact_id: mem.compact_id.clone(),
            replacement_prefix_baseline_tokens: baseline,
            close_input_tokens: mem.close_input_tokens,
        })
    }

    fn open_raw_start(&self, node_id: &NodeId) -> Result<u64, SpineError> {
        let events = &self.ledger.events;
        if let Some(boundary) = events.iter().rev().find_map(|event| match &event.event {
            SpineLedgerEvent::Open {
                child, boundary, ..
            } if child == node_id => Some(*boundary),
            _ => None,
        }) {
            return Ok(boundary);
        }
        let Some(parent) = node_id.parent() else {
            return Err(SpineError::SidecarCorruption(format!(
                "missing open event for {node_id}; node has no parent"
            )));
        };
        if parent.is_root_epoch() && node_id.0.last() == Some(&1) {
            let root_epoch =
                parent.0.first().copied().ok_or_else(|| {
                    SpineError::InvalidEvent("root epoch id is empty".to_string())
                })?;
            let Some(previous_root_epoch) = root_epoch.checked_sub(1) else {
                return Err(SpineError::SidecarCorruption(format!(
                    "missing open event for {node_id}; root epoch {root_epoch} has no previous compact boundary"
                )));
            };
            let compacted_parent = NodeId::root_epoch(previous_root_epoch);
            return events
                .iter()
                .rev()
                .find_map(|event| match &event.event {
                    SpineLedgerEvent::RootCompact { node, boundary, .. }
                        if *node == compacted_parent && parent.child(1) == *node_id =>
                    {
                        Some(*boundary)
                    }
                    _ => None,
                })
                .ok_or_else(|| {
                    SpineError::SidecarCorruption(format!(
                        "missing open event for {node_id}; no root compact boundary for parent {parent}"
                    ))
                });
        }
        Err(SpineError::SidecarCorruption(format!(
            "missing open event for {node_id}; no matching open/root compact event in sidecar"
        )))
    }

    pub(crate) fn materialize_history(
        &self,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<Vec<ResponseItem>, SpineError> {
        self.ensure_jit_enabled("Spine history materialization")?;
        let trim_projection = self.current_trim_projection()?;
        render_parse_stack_to_context_with_trim_projection(
            &self.parse_stack,
            raw_items,
            &trim_projection,
        )
    }

    pub(crate) fn has_pending_tool_request(&self) -> bool {
        !self.ordinary_tool_requests.is_empty()
    }

    pub(crate) fn project_raw_history_with_trim(
        &self,
        raw_items: &[ResponseItem],
    ) -> Result<Vec<ResponseItem>, SpineError> {
        let trim_projection = self.current_trim_projection()?;
        project_raw_history_with_trim_projection(raw_items, &trim_projection)
    }

    pub(crate) fn validate_raw_coverage(
        &self,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<(), SpineError> {
        if !self.jit_enabled {
            return Ok(());
        }
        let (
            spine_control_call_ids,
            spine_tree_call_ids,
            tool_request_call_ids,
            tool_response_call_ids,
        ) = raw_items
            .iter()
            .filter_map(|item| match item.as_ref()? {
                ResponseItem::FunctionCall {
                    call_id,
                    namespace: Some(namespace),
                    name,
                    ..
                } if namespace == SPINE_NAMESPACE && is_spine_parser_control_tool_name(name) => {
                    Some((call_id.clone(), ToolRawItemKind::SpineControlRequest))
                }
                ResponseItem::FunctionCall {
                    call_id,
                    namespace: Some(namespace),
                    name,
                    ..
                } if namespace == SPINE_NAMESPACE && name == SPINE_TOOL_TREE => {
                    Some((call_id.clone(), ToolRawItemKind::SpineTreeRequest))
                }
                item => tool_request_call_id(item)
                    .map(|call_id| (call_id.to_string(), ToolRawItemKind::Request))
                    .or_else(|| {
                        tool_response_call_id(item)
                            .map(|call_id| (call_id.to_string(), ToolRawItemKind::Response))
                    }),
            })
            .fold(
                (
                    BTreeSet::new(),
                    BTreeSet::new(),
                    BTreeSet::new(),
                    BTreeSet::new(),
                ),
                |(
                    mut spine_call_ids,
                    mut spine_tree_call_ids,
                    mut request_call_ids,
                    mut response_call_ids,
                ),
                 (call_id, kind)| {
                    match kind {
                        ToolRawItemKind::SpineControlRequest => {
                            spine_call_ids.insert(call_id.clone());
                            request_call_ids.insert(call_id);
                        }
                        ToolRawItemKind::SpineTreeRequest => {
                            spine_tree_call_ids.insert(call_id.clone());
                            request_call_ids.insert(call_id);
                        }
                        ToolRawItemKind::Request => {
                            request_call_ids.insert(call_id);
                        }
                        ToolRawItemKind::Response => {
                            response_call_ids.insert(call_id);
                        }
                    }
                    (
                        spine_call_ids,
                        spine_tree_call_ids,
                        request_call_ids,
                        response_call_ids,
                    )
                },
            );
        let completed_tool_call_ids = tool_request_call_ids
            .intersection(&tool_response_call_ids)
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut covered = vec![false; raw_items.len()];
        for event in &self.ledger.events {
            if !event.allowed_by(RawMask::new(&self.raw_live))? {
                continue;
            }
            match &event.event {
                SpineLedgerEvent::Msg { raw_ordinal, .. } => {
                    mark_raw_covered(&mut covered, *raw_ordinal)?;
                }
                SpineLedgerEvent::ToolCall { segments } => {
                    for segment in segments {
                        mark_raw_covered(&mut covered, segment.raw_ordinal)?;
                    }
                }
                SpineLedgerEvent::Open {
                    child,
                    boundary,
                    summary,
                    ..
                } => {
                    if !(summary == "root"
                        && child.parent().is_some_and(|parent| parent.is_root_epoch()))
                    {
                        mark_raw_covered(&mut covered, *boundary)?;
                    }
                }
                SpineLedgerEvent::Close { boundary, .. }
                | SpineLedgerEvent::RootCompact { boundary, .. } => {
                    mark_raw_prefix_covered(&mut covered, *boundary)?;
                }
                SpineLedgerEvent::Init { .. } | SpineLedgerEvent::OpenContextBaseline { .. } => {}
            }
        }
        for (index, item) in raw_items.iter().enumerate() {
            if item.as_ref().is_some_and(|item| {
                raw_item_requires_spine_coverage(
                    item,
                    &spine_control_call_ids,
                    &spine_tree_call_ids,
                    &completed_tool_call_ids,
                )
            }) && !covered[index]
            {
                return Err(SpineError::SidecarCorruption(format!(
                    "spine sidecar is missing token coverage for raw ordinal {index}; raw_len={} token_seq={}",
                    raw_items.len(),
                    self.ledger.next_event_seq
                )));
            }
        }
        Ok(())
    }

    pub(crate) fn live_root_compacts(&self) -> Result<Vec<LiveRootCompact>, SpineError> {
        if !self.jit_enabled {
            return Ok(Vec::new());
        }
        let raw_mask = RawMask::new(&self.raw_live);
        let mut compacts = Vec::new();
        for event in &self.ledger.events {
            if event.allowed_by(raw_mask)?
                && let SpineLedgerEvent::RootCompact { boundary, .. } = event.event
            {
                compacts.push(LiveRootCompact {
                    raw_boundary: boundary,
                    token_seq: event.seq,
                });
            }
        }
        Ok(compacts)
    }
}

fn mark_raw_covered(covered: &mut [bool], raw_ordinal: u64) -> Result<(), SpineError> {
    let index = usize::try_from(raw_ordinal)
        .map_err(|_| SpineError::InvalidEvent("raw ordinal overflow".to_string()))?;
    if let Some(slot) = covered.get_mut(index) {
        *slot = true;
    }
    Ok(())
}

fn mark_raw_prefix_covered(covered: &mut [bool], boundary: u64) -> Result<(), SpineError> {
    let boundary = usize::try_from(boundary)
        .map_err(|_| SpineError::InvalidEvent("raw boundary overflow".to_string()))?;
    for slot in covered.iter_mut().take(boundary) {
        *slot = true;
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum ToolRawItemKind {
    SpineControlRequest,
    SpineTreeRequest,
    Request,
    Response,
}

fn completed_toolcall_first_segment(
    toolcall: &CompletedToolCall,
) -> Result<&CompletedToolCallSegment, SpineError> {
    toolcall.segments.first().ok_or_else(|| {
        SpineError::InvalidEvent("completed toolcall must contain at least one segment".to_string())
    })
}

fn validate_completed_toolcall(toolcall: &CompletedToolCall) -> Result<(), SpineError> {
    let segments = &toolcall.segments;
    if segments.is_empty() {
        return Err(SpineError::InvalidEvent(
            "completed toolcall must contain at least one segment".to_string(),
        ));
    }
    let mut has_request = false;
    let mut has_response = false;
    let mut previous_context_index = None;
    let mut previous_raw_ordinal = None;
    for (index, segment) in segments.iter().enumerate() {
        match segment.kind {
            ToolCallSegmentKind::Request => {
                if has_response {
                    return Err(SpineError::InvalidEvent(format!(
                        "completed toolcall request segment {index} appears after a response segment"
                    )));
                }
                has_request = true;
            }
            ToolCallSegmentKind::Response => has_response = true,
        }
        if let Some(previous) = previous_context_index {
            if segment.context_index <= previous {
                return Err(SpineError::InvalidEvent(format!(
                    "completed toolcall segment {index} context_index {} is not strictly after previous context_index {previous}",
                    segment.context_index
                )));
            }
        }
        if let Some(previous) = previous_raw_ordinal {
            if segment.raw_ordinal <= previous {
                return Err(SpineError::InvalidEvent(format!(
                    "completed toolcall segment {index} raw_ordinal {} is not strictly after previous raw_ordinal {previous}",
                    segment.raw_ordinal
                )));
            }
        }
        previous_context_index = Some(segment.context_index);
        previous_raw_ordinal = Some(segment.raw_ordinal);
    }
    if !has_request {
        return Err(SpineError::InvalidEvent(
            "completed toolcall must include at least one request segment".to_string(),
        ));
    }
    if !has_response {
        return Err(SpineError::InvalidEvent(
            "completed toolcall must include at least one response segment".to_string(),
        ));
    }
    let request_segment_count = segments
        .iter()
        .filter(|segment| segment.kind == ToolCallSegmentKind::Request)
        .count();
    if request_segment_count != toolcall.request_call_ids.len() {
        return Err(SpineError::InvalidEvent(format!(
            "completed toolcall request segment count {request_segment_count} does not match request call id count {}",
            toolcall.request_call_ids.len()
        )));
    }
    Ok(())
}

fn raw_item_requires_spine_coverage(
    item: &ResponseItem,
    _spine_control_call_ids: &BTreeSet<String>,
    _spine_tree_call_ids: &BTreeSet<String>,
    completed_tool_call_ids: &BTreeSet<String>,
) -> bool {
    match item {
        ResponseItem::FunctionCall {
            call_id,
            namespace: Some(namespace),
            name,
            ..
        } if namespace == SPINE_NAMESPACE && is_spine_parser_control_tool_name(name) => {
            completed_tool_call_ids.contains(call_id)
        }
        ResponseItem::FunctionCall {
            call_id,
            namespace: Some(namespace),
            name,
            ..
        } if namespace == SPINE_NAMESPACE && name == SPINE_TOOL_TREE => {
            completed_tool_call_ids.contains(call_id)
        }
        ResponseItem::Other | ResponseItem::CompactionTrigger => false,
        item => {
            if let Some(call_id) = tool_response_call_id(item) {
                return completed_tool_call_ids.contains(call_id);
            }
            if let Some(call_id) = tool_request_call_id(item) {
                return completed_tool_call_ids.contains(call_id);
            }
            true
        }
    }
}

fn tool_request_call_id(item: &ResponseItem) -> Option<&str> {
    match item {
        ResponseItem::FunctionCall { call_id, .. }
        | ResponseItem::CustomToolCall { call_id, .. } => Some(call_id.as_str()),
        ResponseItem::ToolSearchCall {
            call_id: Some(call_id),
            ..
        } => Some(call_id.as_str()),
        _ => None,
    }
}

fn tool_response_call_id(item: &ResponseItem) -> Option<&str> {
    match item {
        ResponseItem::FunctionCallOutput { call_id, .. }
        | ResponseItem::CustomToolCallOutput { call_id, .. } => Some(call_id.as_str()),
        ResponseItem::ToolSearchOutput {
            call_id: Some(call_id),
            ..
        } => Some(call_id.as_str()),
        _ => None,
    }
}

impl SpineCompactSourcePlanEntry {
    pub(crate) fn visible_response_item(&self) -> ResponseItem {
        match &self.kind {
            SpineCompactSourceEntryKind::RawResponseItem { item, .. } => item.clone(),
            SpineCompactSourceEntryKind::ChildMemory { body, .. } => memory_response_item(body),
        }
    }
}

fn collect_source_plan_entries_from_visible_refs(
    visible_refs: &[crate::spine::render::VisibleItemRef],
    raw_context_items: &[ResponseItem],
) -> Result<Vec<SpineCompactSourcePlanEntry>, SpineError> {
    let mut entries = Vec::with_capacity(visible_refs.len());
    for visible_ref in visible_refs {
        match &visible_ref.source {
            VisibleItemSource::RawResponseItem {
                raw_ordinal,
                from_user,
                user_anchor,
            } => collect_source_plan_entry_from_response_item(
                *raw_ordinal,
                visible_ref.context_index,
                *from_user,
                *user_anchor,
                raw_context_items,
                &mut entries,
            )?,
            VisibleItemSource::ToolCallSegment { raw_ordinal, kind } => {
                let _ = kind;
                collect_source_plan_entry_from_response_item(
                    *raw_ordinal,
                    visible_ref.context_index,
                    false,
                    None,
                    raw_context_items,
                    &mut entries,
                )?;
            }
            VisibleItemSource::MemoryRef { memory, .. } => {
                let source_ordinal = entries.len();
                let body = read_memory_ref_body(memory)?;
                let visible_item = memory_response_item(&body);
                let source_hash = hash_response_items(std::slice::from_ref(&visible_item))?;
                entries.push(SpineCompactSourcePlanEntry {
                    context_index: visible_ref.context_index,
                    source_ordinal,
                    source_hash,
                    kind: SpineCompactSourceEntryKind::ChildMemory {
                        node_id: memory.node_id.clone(),
                        compact_id: memory.compact_id.clone(),
                        source_raw_range: memory.source_raw_range.clone(),
                        body,
                        body_hash: memory.body_hash.clone(),
                    },
                });
            }
            VisibleItemSource::MemorySeg { memory_id, .. } => {
                return Err(SpineError::CompactFailure(format!(
                    "spine.close source plan cannot trust SegRef::Memory {memory_id} without MemoryRef body_hash provenance"
                )));
            }
        }
    }
    Ok(entries)
}

fn collect_source_plan_entry_from_response_item(
    raw_ordinal: u64,
    context_index: usize,
    from_user: bool,
    user_anchor: Option<u64>,
    raw_context_items: &[ResponseItem],
    entries: &mut Vec<SpineCompactSourcePlanEntry>,
) -> Result<(), SpineError> {
    let source_ordinal = entries.len();
    let item = raw_context_items
        .get(context_index)
        .cloned()
        .ok_or_else(|| {
            SpineError::CompactFailure(format!(
                "spine.close source plan raw item context_index {context_index} exceeds host history length {}",
                raw_context_items.len()
            ))
        })?;
    let source_hash = hash_response_items(std::slice::from_ref(&item))?;
    entries.push(SpineCompactSourcePlanEntry {
        context_index,
        source_ordinal,
        source_hash,
        kind: SpineCompactSourceEntryKind::RawResponseItem {
            item,
            raw_ordinal,
            from_user,
            user_anchor,
        },
    });
    Ok(())
}

fn validate_model_node_memory(memory: &str) -> Result<(), SpineError> {
    if memory.trim().is_empty() {
        return Err(SpineError::ToolUse(
            "spine.close/next memory must not be empty".to_string(),
        ));
    }
    Ok(())
}

fn user_anchor_refs_in_memory(memory: &str) -> Result<BTreeSet<u64>, SpineError> {
    let bytes = memory.as_bytes();
    let mut refs = BTreeSet::new();
    let mut offset = 0usize;
    while let Some(relative_start) = memory[offset..].find("[U") {
        let start = offset
            .checked_add(relative_start)
            .ok_or_else(|| SpineError::InvalidEvent("user anchor scan overflow".to_string()))?;
        let digits_start = start
            .checked_add(2)
            .ok_or_else(|| SpineError::InvalidEvent("user anchor scan overflow".to_string()))?;
        let mut digits_end = digits_start;
        while digits_end < bytes.len() && bytes[digits_end].is_ascii_digit() {
            digits_end += 1;
        }
        if digits_end > digits_start && bytes.get(digits_end) == Some(&b']') {
            let anchor = memory[digits_start..digits_end]
                .parse::<u64>()
                .map_err(|_| {
                    SpineError::ToolUse(
                        "spine.close/next memory contains invalid user anchor".to_string(),
                    )
                })?;
            refs.insert(anchor);
            offset = digits_end
                .checked_add(1)
                .ok_or_else(|| SpineError::InvalidEvent("user anchor scan overflow".to_string()))?;
        } else {
            offset = start
                .checked_add(2)
                .ok_or_else(|| SpineError::InvalidEvent("user anchor scan overflow".to_string()))?;
        }
    }
    Ok(refs)
}

fn validate_source_plan_context_index(
    source_ordinal: usize,
    context_index: usize,
    suffix_start: usize,
    source_context_end: usize,
    previous_context_index: &mut Option<usize>,
) -> Result<(), SpineError> {
    if context_index < suffix_start {
        return Err(SpineError::CompactFailure(format!(
            "spine.close source plan entry ordinal {source_ordinal} context_index {context_index} precedes suffix start {suffix_start}"
        )));
    }
    if context_index >= source_context_end {
        return Err(SpineError::CompactFailure(format!(
            "spine.close source plan entry ordinal {source_ordinal} context_index {context_index} is outside source context range [{suffix_start}..{source_context_end})"
        )));
    }
    if let Some(previous) = *previous_context_index {
        if context_index <= previous {
            return Err(SpineError::CompactFailure(format!(
                "spine.close source plan entry ordinal {source_ordinal} context_index {context_index} is not strictly after previous context_index {previous}"
            )));
        }
    }
    *previous_context_index = Some(context_index);
    Ok(())
}

fn close_event_boundary(event: &SpineLedgerEvent) -> Result<u64, SpineError> {
    match event {
        SpineLedgerEvent::Close { boundary, .. } => Ok(*boundary),
        _ => Err(SpineError::Invariant(
            "close commit marker requested for non-close event".to_string(),
        )),
    }
}

fn replacement_prefix_baseline_tokens(mem: &MemRecord) -> Option<i64> {
    if mem.context_start == 0 {
        return Some(0);
    }
    mem.open_context_tokens
}

fn pending_memory_context_accounting_from_store(
    store: &SpineStore,
) -> Result<Option<PendingMemoryContextAccounting>, SpineError> {
    let accounted = store
        .mem_accounting()?
        .into_iter()
        .map(|record| record.compact_id)
        .collect::<BTreeSet<_>>();
    let mut pending_by_id = BTreeMap::new();
    for witness in store.mem_accounting_witnesses()? {
        match witness {
            MemoryContextAccountingWitnessRecord::Pending {
                compact_id,
                replacement_prefix_baseline_tokens,
                close_input_tokens,
            } => {
                if !accounted.contains(&compact_id) {
                    pending_by_id.insert(
                        compact_id.clone(),
                        PendingMemoryContextAccounting {
                            compact_id,
                            replacement_prefix_baseline_tokens,
                            close_input_tokens,
                        },
                    );
                }
            }
            MemoryContextAccountingWitnessRecord::Consumed { compact_id, .. } => {
                pending_by_id.remove(&compact_id);
            }
        }
    }
    for compact_id in accounted {
        pending_by_id.remove(&compact_id);
    }
    match pending_by_id.len() {
        0 => Ok(None),
        1 => Ok(pending_by_id.into_values().next()),
        _ => Err(SpineError::InvalidStore(
            "multiple unconsumed Spine memory context accounting pending witnesses".to_string(),
        )),
    }
}

fn close_commit_marker(
    seq: u64,
    mem: &MemRecord,
    kind: SpineCommitKindMarker,
    raw_boundary: u64,
    width: u64,
) -> Result<SpineCommitMarker, SpineError> {
    if kind == SpineCommitKindMarker::RootCompact {
        return Err(SpineError::Invariant(
            "root compact marker requested from close marker builder".to_string(),
        ));
    }
    Ok(SpineCommitMarker {
        version: COMMIT_MARKER_VERSION,
        op_id: format!("{}:{}", commit_marker_kind_label(kind), mem.compact_id),
        kind,
        token_seq_start: seq,
        token_seq_end: seq.checked_add(width).ok_or_else(|| {
            SpineError::InvalidEvent("Spine commit marker token seq overflow".to_string())
        })?,
        raw_boundary,
        raw_live_hash: None,
        memory_refs: vec![commit_memory_ref(mem)],
    })
}

fn root_compact_commit_marker(seq: u64, mem: &MemRecord) -> Result<SpineCommitMarker, SpineError> {
    Ok(SpineCommitMarker {
        version: COMMIT_MARKER_VERSION,
        op_id: format!("root_compact:{}", mem.compact_id),
        kind: SpineCommitKindMarker::RootCompact,
        token_seq_start: seq,
        token_seq_end: seq.checked_add(1).ok_or_else(|| {
            SpineError::InvalidEvent("Spine commit marker token seq overflow".to_string())
        })?,
        raw_boundary: mem.raw_end,
        raw_live_hash: mem.raw_live_hash.clone(),
        memory_refs: vec![commit_memory_ref(mem)],
    })
}

fn commit_marker_kind_label(kind: SpineCommitKindMarker) -> &'static str {
    match kind {
        SpineCommitKindMarker::Close => "close",
        SpineCommitKindMarker::CloseThenOpen => "close_then_open",
        SpineCommitKindMarker::RootCompact => "root_compact",
    }
}

fn commit_memory_ref(mem: &MemRecord) -> SpineCommitMemoryRef {
    SpineCommitMemoryRef {
        compact_id: mem.compact_id.clone(),
        kind: mem.kind,
        node: mem.node.clone(),
        raw_start: mem.raw_start,
        raw_end: mem.raw_end,
        context_start: mem.context_start,
        context_end: mem.context_end,
        raw_live_hash: mem.raw_live_hash.clone(),
        body_path: mem.body_path.clone(),
        body_hash: mem.body_hash.clone(),
    }
}

fn mem_record_matches(existing: &MemRecord, expected: &MemRecord) -> bool {
    existing.compact_id == expected.compact_id
        && existing.kind == expected.kind
        && existing.node == expected.node
        && existing.raw_start == expected.raw_start
        && existing.raw_end == expected.raw_end
        && existing.context_start == expected.context_start
        && existing.context_end == expected.context_end
        && existing.raw_live_hash == expected.raw_live_hash
        && existing.open_input_tokens == expected.open_input_tokens
        && existing.close_input_tokens == expected.close_input_tokens
        && existing.open_context_tokens == expected.open_context_tokens
        && existing.close_context_tokens == expected.close_context_tokens
        && existing.closed_source_suffix_tokens == expected.closed_source_suffix_tokens
        && existing.closed_memory_context_tokens == expected.closed_memory_context_tokens
        && existing.open_context_source == expected.open_context_source
        && existing.memory_output_tokens == expected.memory_output_tokens
        && existing.body_path == expected.body_path
        && existing.body_hash == expected.body_hash
}

pub(crate) fn is_user_message(item: &ResponseItem) -> bool {
    matches!(item, ResponseItem::Message { role, .. } if role == "user")
}

pub(crate) fn is_real_user_message(item: &ResponseItem) -> bool {
    let ResponseItem::Message { role, content, .. } = item else {
        return false;
    };
    role == "user"
        && !content
            .iter()
            .any(crate::context::is_contextual_user_fragment)
        && !content.iter().any(is_spine_memory_fragment)
}

fn is_spine_memory_fragment(content_item: &codex_protocol::models::ContentItem) -> bool {
    let codex_protocol::models::ContentItem::InputText { text } = content_item else {
        return false;
    };
    text.trim_start().starts_with("<spine_memory>")
}

fn is_spine_parser_control_tool_name(name: &str) -> bool {
    matches!(name, SPINE_TOOL_OPEN | SPINE_TOOL_CLOSE | SPINE_TOOL_NEXT)
}

#[cfg(test)]
pub(crate) fn is_spine_close_like_tool_name(name: &str) -> bool {
    matches!(name, SPINE_TOOL_CLOSE | SPINE_TOOL_NEXT)
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
