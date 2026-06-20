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
use crate::spine::checkpoint::build_checkpoint;
use crate::spine::compact_checkpoint::build_compact_checkpoint;
use crate::spine::io::hash_raw_live;
use crate::spine::io::hash_response_items;
use crate::spine::io::sha1_hex;
#[cfg(test)]
use crate::spine::model::COMMIT_MARKER_VERSION;
use crate::spine::model::ContextBaselineSource;
use crate::spine::model::ControlSymbol;
use crate::spine::model::LoggedPressureEvent;
use crate::spine::model::LoggedSpineLedgerEvent;
use crate::spine::model::LoggedTrimEvent;
use crate::spine::model::MemKind;
use crate::spine::model::MemRecord;
use crate::spine::model::NodeId;
use crate::spine::model::RawMask;
#[cfg(test)]
use crate::spine::model::SegRef;
use crate::spine::model::SpineCommitKindMarker;
use crate::spine::model::SpineCommitMarker;
#[cfg(test)]
use crate::spine::model::SpineCommitMemoryRef;
use crate::spine::model::SpineLedgerEvent;
use crate::spine::model::SpineToken;
use crate::spine::model::SpineTreeNode;
use crate::spine::model::Symbol;
use crate::spine::model::ToolCallSegmentKind;
use crate::spine::model::TreeMeta;
#[cfg(test)]
use crate::spine::model::TrimEvent;
use crate::spine::parse_stack::ParseStack;
use crate::spine::parse_stack::PreparedRootEpochReduction;
use crate::spine::parse_stack::PreparedTaskTreeReduction;
#[cfg(test)]
use crate::spine::parse_stack::parse_stack_msg_leaf_count;
#[cfg(test)]
use crate::spine::parse_stack::parse_stack_toolcall_leaf_count;
use crate::spine::render::memory_response_item;
use crate::spine::render::project_spine_tree_nodes_visible_items;
#[cfg(test)]
use crate::spine::render::render_parse_stack_to_context;
use crate::spine::render::render_parse_stack_to_context_with_memory_body_and_trim_projection;
use crate::spine::render::render_parse_stack_to_context_with_trim_projection;
use crate::spine::store::BODY_DIR;
use crate::spine::store::SpineStore;

mod accounting;
mod coverage;
mod load;
mod observe;
mod replay;
mod support;
mod trim;

#[cfg(test)]
use crate::spine::model::commit_marker_structural_event_seqs;
#[cfg(test)]
use replay::ReplayCommitClassification;
#[cfg(test)]
use replay::classify_commit_marker_for_replay;
use replay::next_event_seq_from;
use replay::next_pressure_seq_from;
use replay::next_trim_seq_from;
pub(crate) use replay::trim_projection_from_events_for_checkpoint;
use support::close_commit_marker;
use support::close_event_boundary;
use support::collect_source_plan_entries_from_visible_refs;
use support::completed_toolcall_first_segment;
pub(crate) use support::is_real_user_message;
#[cfg(test)]
pub(crate) use support::is_spine_close_like_tool_name;
pub(crate) use support::is_user_message;
use support::mem_record_matches;
use support::root_compact_commit_marker;
use support::user_anchor_refs_in_memory;
use support::validate_model_node_memory;
use support::validate_source_plan_context_index;

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
