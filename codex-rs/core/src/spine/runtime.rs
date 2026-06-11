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
use crate::spine::model::MemKind;
use crate::spine::model::MemRecord;
use crate::spine::model::NodeId;
use crate::spine::model::PressureEvent;
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
use crate::spine::model::commit_marker_structural_event_seqs;
use crate::spine::parse_stack::ParseStack;
use crate::spine::parse_stack::event_to_token;
use crate::spine::parse_stack::parse_stack_from_events_with_forced_events;
#[cfg(test)]
use crate::spine::parse_stack::parse_stack_msg_leaf_count;
#[cfg(test)]
use crate::spine::parse_stack::parse_stack_toolcall_leaf_count;
use crate::spine::render::VisibleItemSource;
use crate::spine::render::memory_response_item;
use crate::spine::render::project_spine_tree_nodes_visible_items;
use crate::spine::render::read_memory_ref_body;
use crate::spine::render::render_parse_stack_to_context;
use crate::spine::render::render_parse_stack_to_context_with_memory_body;
use crate::spine::store::BODY_DIR;
use crate::spine::store::SpineStore;

pub(crate) const SPINE_NAMESPACE: &str = "spine";
pub(crate) const SPINE_TOOL_TREE: &str = "tree";
pub(crate) const SPINE_TOOL_OPEN: &str = "open";
pub(crate) const SPINE_TOOL_CLOSE: &str = "close";
pub(crate) const SPINE_TOOL_NEXT: &str = "next";
pub(crate) const SPINE_CONTROL_MULTI_CALL_REJECTION_PREFIX: &str =
    "Spine parser-control tools are mutually exclusive within one model response;";

#[derive(Clone, Debug)]
pub(crate) struct SpineRuntime {
    store: SpineStore,
    ledger: SpineLedgerCache,
    parse_stack: ParseStack,
    raw_len: u64,
    raw_live: Vec<bool>,
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
    pressure_baselines: BTreeMap<NodeId, OpenContextBaseline>,
}

#[derive(Clone, Debug)]
struct SpineLedgerCache {
    events: Vec<LoggedSpineLedgerEvent>,
    pressure_events: Vec<LoggedPressureEvent>,
    next_event_seq: u64,
    next_pressure_seq: u64,
}

impl SpineLedgerCache {
    fn new(
        events: Vec<LoggedSpineLedgerEvent>,
        pressure_events: Vec<LoggedPressureEvent>,
    ) -> Result<Self, SpineError> {
        let next_event_seq = next_event_seq_from(&events)?;
        let next_pressure_seq = next_pressure_seq_from(&pressure_events)?;
        Ok(Self {
            events,
            pressure_events,
            next_event_seq,
            next_pressure_seq,
        })
    }
}

#[derive(Clone, Debug)]
struct OpenRequestAnchor {
    raw_ordinal: u64,
    context_index: u64,
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
        instruction: Option<String>,
    },
    NextSugar {
        call_id: String,
        summary: String,
        instruction: Option<String>,
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
    staged_parse_stack: ParseStack,
}

struct PreparedRootCompactCommit {
    result: SpineRootCompactResult,
    mem: MemRecord,
    memory_body: String,
    compact_checkpoint: Option<crate::spine::compact_checkpoint::SpineCompactCheckpoint>,
    root_compact_event: SpineLedgerEvent,
    staged_parse_stack: ParseStack,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OpenContextBaseline {
    context_tokens: i64,
    input_tokens: Option<i64>,
    source: ContextBaselineSource,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SpineCommitKind {
    Open {
        open_request_index: usize,
    },
    Close {
        suffix_start: usize,
        replacement: Vec<ResponseItem>,
        toolcall_start: usize,
    },
    CloseThenOpen {
        suffix_start: usize,
        replacement: Vec<ResponseItem>,
        toolcall_start: usize,
        open_index: usize,
    },
}

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
        instruction: Option<String>,
        next_summary: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpineCloseCompact {
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
    pub(crate) input_tokens: Option<i64>,
    pub(crate) context_tokens: Option<i64>,
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
    pub(crate) baseline_tokens: Option<i64>,
    pub(crate) baseline_source: Option<codex_protocol::spine_tree::SpineNodeContextBaselineSource>,
}

#[derive(Clone, Debug)]
pub(crate) struct SpineRootCompactResult {
    pub(crate) materialized: Vec<ResponseItem>,
    pub(crate) raw_boundary: u64,
    pub(crate) token_seq_after: u64,
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
    initial_tree_snapshot_emitted: bool,
    invalid: Option<String>,
}

impl SpineSessionState {
    pub(crate) fn new() -> Self {
        Self {
            raw_len: 0,
            runtime: None,
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

    pub(crate) fn raw_len(&self) -> u64 {
        self.raw_len
    }

    pub(crate) fn set_replayed(
        &mut self,
        raw_len: u64,
        runtime: Option<SpineRuntime>,
    ) -> Result<(), SpineError> {
        self.raw_len = raw_len;
        self.runtime = runtime;
        self.initial_tree_snapshot_emitted = false;
        self.invalid = None;
        Ok(())
    }

    pub(crate) fn invalidate(&mut self, reason: impl Into<String>) {
        self.invalid = Some(reason.into());
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
            self.runtime = Some(SpineRuntime::load_or_create(rollout_path, self.raw_len)?);
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
        let snapshot = runtime.build_tree_snapshot()?;
        self.initial_tree_snapshot_emitted = true;
        Ok(Some(snapshot))
    }
}

impl SpineRuntime {
    pub(crate) fn load_or_create(rollout_path: &Path, raw_len: u64) -> Result<Self, SpineError> {
        let store = if SpineStore::has_for_rollout(rollout_path)? {
            SpineStore::for_rollout(rollout_path)?
        } else {
            SpineStore::create_for_rollout(rollout_path)?
        };
        if !store.tree_path().exists() {
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
        Self::load(store, raw_len)
    }

    pub(crate) fn load_for_rollout_items(
        rollout_path: &Path,
        raw_items: &[Option<ResponseItem>],
        rollback_cuts: &[usize],
    ) -> Result<Option<Self>, SpineError> {
        if !SpineStore::has_for_rollout(rollout_path)? {
            return Ok(None);
        }
        let runtime = Self::load_with_raw_live_for_rollout(
            SpineStore::for_rollout(rollout_path)?,
            raw_items.iter().map(Option::is_some).collect(),
            rollback_cuts,
            rollout_path,
            raw_items,
        )?;
        runtime.validate_raw_coverage(raw_items)?;
        Ok(Some(runtime))
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
        if let Some(checkpoint) = checkpoint.as_ref() {
            validate_checkpoint(checkpoint, rollout_path, &raw_live, raw_items)?;
            return Self::load_with_rollback_checkpoint(store, raw_live, checkpoint);
        }
        if let Some(checkpoint) = store.resume_checkpoint(raw_live.len())? {
            validate_checkpoint(&checkpoint, rollout_path, &raw_live, raw_items)?;
            Self::validate_checkpoint_parse_stack_prefix(&store, &raw_live, &checkpoint)?;
        }
        Self::load_with_raw_live(store, raw_live)
    }

    fn validate_checkpoint_parse_stack_prefix(
        store: &SpineStore,
        raw_live: &[bool],
        checkpoint: &SpineCheckpoint,
    ) -> Result<(), SpineError> {
        let ledger = SpineLedgerCache::new(store.events()?, store.pressure_events()?)?;
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
        let ledger = SpineLedgerCache::new(store.events()?, store.pressure_events()?)?;
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
        let replay_structural_seq = event_limit.unwrap_or(ledger.next_event_seq);
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
        let pressure_baselines = replay_pressure_baselines(
            &parse_stack,
            &ledger.pressure_events,
            &raw_live,
            replay_structural_seq,
            None,
            false,
        );
        Ok(Self {
            store,
            ledger,
            parse_stack,
            raw_len: u64::try_from(raw_live.len())
                .map_err(|_| SpineError::InvalidEvent("raw item count overflow".to_string()))?,
            raw_live,
            open_requests: BTreeMap::new(),
            control_call_ids: BTreeSet::new(),
            tree_call_ids: BTreeSet::new(),
            ordinary_tool_requests: BTreeMap::new(),
            #[cfg(test)]
            pending_tool_responses: BTreeMap::new(),
            pending: None,
            pressure_baselines,
        })
    }

    fn load_with_rollback_checkpoint(
        store: SpineStore,
        raw_live: Vec<bool>,
        checkpoint: &SpineCheckpoint,
    ) -> Result<Self, SpineError> {
        let ledger = SpineLedgerCache::new(store.events()?, store.pressure_events()?)?;
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
        let pressure_baselines = replay_pressure_baselines(
            &parse_stack,
            &ledger.pressure_events,
            &raw_live,
            ledger.next_event_seq,
            checkpoint.pressure_seq_watermark,
            true,
        );
        Ok(Self {
            store,
            ledger,
            parse_stack,
            raw_len: u64::try_from(raw_live.len())
                .map_err(|_| SpineError::InvalidEvent("raw item count overflow".to_string()))?,
            raw_live,
            open_requests: BTreeMap::new(),
            control_call_ids: BTreeSet::new(),
            tree_call_ids: BTreeSet::new(),
            ordinary_tool_requests: BTreeMap::new(),
            #[cfg(test)]
            pending_tool_responses: BTreeMap::new(),
            pending: None,
            pressure_baselines,
        })
    }

    #[cfg(test)]
    pub(crate) fn render_tree(&self) -> Result<String, SpineError> {
        self.parse_stack.render_tree()
    }

    pub(crate) fn render_tree_with_context_annotations(
        &self,
        annotations: &BTreeMap<NodeId, String>,
    ) -> Result<String, SpineError> {
        self.parse_stack
            .render_tree_with_context_annotations(annotations)
    }

    pub(crate) fn build_tree_snapshot(&self) -> Result<SpineTreeUpdateEvent, SpineError> {
        let nodes = self.parse_stack.tree_snapshot_nodes()?;
        let active_node_id = self.parse_stack.current_cursor_id()?.as_path();
        Ok(SpineTreeUpdateEvent {
            snapshot_seq: self.ledger.next_event_seq,
            active_node_id,
            nodes,
        })
    }

    pub(crate) fn current_open_index(&self) -> Result<usize, SpineError> {
        Ok(self.parse_stack.current_open_meta()?.index)
    }

    #[cfg(test)]
    pub(crate) fn current_open_input_tokens(&self) -> Option<i64> {
        self.parse_stack
            .current_open_meta_opt()
            .and_then(|meta| meta.open_input_tokens)
    }

    #[cfg(test)]
    pub(crate) fn current_open_context_tokens(&self) -> Option<i64> {
        self.current_open_context_baseline()
            .map(|baseline| baseline.context_tokens)
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
            .and_then(|meta| self.open_context_baseline_for(meta))
    }

    pub(crate) fn open_node_context_projections(&self) -> Vec<SpineOpenNodeContextProjection> {
        self.parse_stack
            .live_open_metas()
            .into_iter()
            .map(|meta| {
                let baseline = self.open_context_baseline_for(meta);
                SpineOpenNodeContextProjection {
                    node_id: meta.id.clone(),
                    baseline_tokens: baseline.map(|baseline| baseline.context_tokens),
                    baseline_source: baseline
                        .map(|baseline| baseline.source)
                        .map(protocol_context_baseline_source),
                }
            })
            .collect()
    }

    fn open_context_baseline_for(&self, meta: &TreeMeta) -> Option<OpenContextBaseline> {
        meta.open_context_tokens
            .map(|context_tokens| OpenContextBaseline {
                context_tokens,
                input_tokens: meta.open_input_tokens,
                source: meta
                    .open_context_source
                    .unwrap_or(ContextBaselineSource::ProviderAtOpen),
            })
            .or_else(|| self.pressure_baselines.get(&meta.id).copied())
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

    pub(crate) fn ensure_current_open_context_baseline(
        &mut self,
        current_context_tokens: i64,
        current_input_tokens: Option<i64>,
        estimated_live_suffix_tokens: Option<i64>,
        observed_context_index: usize,
    ) -> Result<bool, SpineError> {
        let Some(open_meta) = self.parse_stack.current_open_meta_opt() else {
            return Ok(false);
        };
        if open_meta.open_context_tokens.is_some()
            || self.pressure_baselines.contains_key(&open_meta.id)
        {
            return Ok(false);
        }

        let estimated_live_suffix_tokens = estimated_live_suffix_tokens.unwrap_or(0).max(0);
        let context_tokens = current_context_tokens.saturating_sub(estimated_live_suffix_tokens);
        let node = open_meta.id.clone();
        let event = PressureEvent::OpenContextBaseline {
            node: node.clone(),
            open_structural_seq: open_structural_seq_for(&self.ledger.events, &node),
            observed_structural_seq: self.ledger.next_event_seq,
            observed_raw_ordinal: self.raw_len,
            observed_raw_live_hash: Some(hash_raw_live(&self.raw_live)),
            observed_context_index,
            context_tokens,
            input_tokens: current_input_tokens,
            source: ContextBaselineSource::EstimatedFromLiveSuffix,
            estimated_live_suffix_tokens: Some(estimated_live_suffix_tokens),
        };
        self.append_cached_pressure_event(event)?;
        self.pressure_baselines.insert(
            node,
            OpenContextBaseline {
                context_tokens,
                input_tokens: current_input_tokens,
                source: ContextBaselineSource::EstimatedFromLiveSuffix,
            },
        );
        Ok(true)
    }

    pub(crate) fn observe_context_item(
        &mut self,
        raw_ordinal: u64,
        context_index: usize,
        item: &ResponseItem,
    ) -> Result<(), SpineError> {
        let context_index = u64::try_from(context_index)
            .map_err(|_| SpineError::InvalidEvent("context index overflow".to_string()))?;
        let msg = PendingMsg {
            raw_ordinal,
            context_index,
            from_user: is_user_message(item),
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
        let (event, segments) = self.completed_toolcall_parts(&toolcall)?;
        self.append_cached_event(event)?;
        self.push_completed_toolcall_token(segments)?;
        self.clear_completed_toolcall_anchors(&toolcall);
        Ok(())
    }

    pub(crate) fn abort_pending_and_observe_completed_toolcall(
        &mut self,
        call_id: &str,
        toolcall: CompletedToolCall,
    ) -> Result<bool, SpineError> {
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
        self.append_cached_event(event)?;
        self.parse_stack = staged_parse_stack;
        self.pending = None;
        self.clear_completed_toolcall_anchors(&toolcall);
        Ok(true)
    }

    pub(crate) fn commit_completed_toolcall_as_ordinary(
        &mut self,
        call_id: &str,
        toolcall: CompletedToolCall,
    ) -> Result<bool, SpineError> {
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.call_id() == call_id)
        {
            return self.abort_pending_and_observe_completed_toolcall(call_id, toolcall);
        }
        let (event, segments) = self.completed_toolcall_parts(&toolcall)?;
        let mut staged_parse_stack = self.parse_stack.clone();
        staged_parse_stack.shift(SpineToken::ToolCall { segments }, &self.archive())?;
        self.append_cached_event(event)?;
        self.parse_stack = staged_parse_stack;
        self.clear_completed_toolcall_anchors(&toolcall);
        Ok(false)
    }

    pub(crate) fn observe_recorded_tool_output_group_as_completed_toolcall(
        &mut self,
        tool_responses: &[(String, u64, usize)],
    ) -> Result<(), SpineError> {
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
        self.observe_completed_toolcall(CompletedToolCall {
            call_id,
            request_call_ids,
            segments,
        })
    }

    pub(crate) fn checkpoint_before_user_msg(
        &self,
        rollout_path: &Path,
        raw_ordinal: u64,
        context: &[ResponseItem],
    ) -> Result<(), SpineError> {
        let checkpoint = build_checkpoint(
            rollout_path,
            raw_ordinal,
            self.ledger.next_event_seq,
            self.pressure_seq_watermark()?,
            &self.raw_live,
            &self.parse_stack,
            context,
        )?;
        self.store.write_checkpoint(&checkpoint)
    }

    pub(crate) fn checkpoint_initial(
        &self,
        rollout_path: &Path,
        context: &[ResponseItem],
    ) -> Result<(), SpineError> {
        let mut checkpoint = build_checkpoint(
            rollout_path,
            0,
            self.ledger.next_event_seq,
            self.pressure_seq_watermark()?,
            &self.raw_live,
            &self.parse_stack,
            context,
        )?;
        checkpoint.checkpoint_id = "initial".to_string();
        self.store.write_initial_checkpoint(&checkpoint)
    }

    fn pressure_seq_watermark(&self) -> Result<Option<u64>, SpineError> {
        Ok(self.ledger.next_pressure_seq.checked_sub(1))
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

    fn append_cached_pressure_event(&mut self, event: PressureEvent) -> Result<u64, SpineError> {
        let pressure_seq = self.ledger.next_pressure_seq;
        let next_pressure_seq = pressure_seq
            .checked_add(1)
            .ok_or_else(|| SpineError::InvalidEvent("spine pressure seq overflow".to_string()))?;
        let logged = LoggedPressureEvent {
            pressure_seq,
            event,
        };
        self.store.append_logged_pressure_event(&logged)?;
        self.ledger.pressure_events.push(logged);
        self.ledger.next_pressure_seq = next_pressure_seq;
        Ok(pressure_seq)
    }

    fn append_msg_event(&mut self, msg: &PendingMsg) -> Result<u64, SpineError> {
        self.append_cached_event(SpineLedgerEvent::Msg {
            raw_ordinal: msg.raw_ordinal,
            context_index: msg.context_index,
            from_user: msg.from_user,
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

    pub(crate) fn stage_close(
        &mut self,
        call_id: String,
        instruction: Option<String>,
    ) -> Result<(), SpineError> {
        self.ensure_no_pending_transition()?;
        if !self.control_call_ids.contains(&call_id) {
            return Err(SpineError::Operation(format!(
                "missing spine.close request anchor for call_id={call_id}"
            )));
        }
        self.stage(PendingTransition::Close {
            call_id,
            instruction,
        })
    }

    pub(crate) fn stage_next(
        &mut self,
        call_id: String,
        summary: String,
        instruction: Option<String>,
    ) -> Result<(), SpineError> {
        self.ensure_no_pending_transition()?;
        let summary = summary.trim().to_string();
        if summary.is_empty() {
            return Err(SpineError::ToolUse(
                "spine.next summary must not be empty".to_string(),
            ));
        }
        if !self.control_call_ids.contains(&call_id) {
            return Err(SpineError::Operation(format!(
                "missing spine.next request anchor for call_id={call_id}"
            )));
        }
        self.stage(PendingTransition::NextSugar {
            call_id,
            summary,
            instruction,
        })
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
        close_compact: Option<SpineCloseCompact>,
    ) -> Result<Option<SpineCommitKind>, SpineError> {
        let completed_toolcall = self.observed_completed_toolcall(call_id)?;
        self.maybe_commit_output_impl(
            call_id,
            close_compact,
            SpineTokenBaselines::default(),
            completed_toolcall,
        )
    }

    #[cfg(test)]
    pub(crate) fn maybe_commit_output_with_open_input_tokens(
        &mut self,
        call_id: &str,
        close_compact: Option<SpineCloseCompact>,
        input_tokens: Option<i64>,
    ) -> Result<Option<SpineCommitKind>, SpineError> {
        let completed_toolcall = self.observed_completed_toolcall(call_id)?;
        self.maybe_commit_output_impl(
            call_id,
            close_compact,
            SpineTokenBaselines {
                input_tokens,
                context_tokens: input_tokens,
            },
            completed_toolcall,
        )
    }

    #[cfg(test)]
    pub(crate) fn maybe_commit_output_with_token_baselines(
        &mut self,
        call_id: &str,
        close_compact: Option<SpineCloseCompact>,
        token_baselines: SpineTokenBaselines,
    ) -> Result<Option<SpineCommitKind>, SpineError> {
        let completed_toolcall = self.observed_completed_toolcall(call_id)?;
        self.maybe_commit_output_impl(call_id, close_compact, token_baselines, completed_toolcall)
    }

    pub(crate) fn maybe_commit_output_with_toolcall(
        &mut self,
        call_id: &str,
        close_compact: Option<SpineCloseCompact>,
        token_baselines: SpineTokenBaselines,
        completed_toolcall: CompletedToolCall,
    ) -> Result<Option<SpineCommitKind>, SpineError> {
        self.maybe_commit_output_impl(
            call_id,
            close_compact,
            token_baselines,
            Some(completed_toolcall),
        )
    }

    fn maybe_commit_output_impl(
        &mut self,
        call_id: &str,
        close_compact: Option<SpineCloseCompact>,
        token_baselines: SpineTokenBaselines,
        completed_toolcall: Option<CompletedToolCall>,
    ) -> Result<Option<SpineCommitKind>, SpineError> {
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
            )?,
            PendingTransition::Close { instruction, .. } => self.commit_close_pending(
                instruction,
                close_compact,
                token_baselines,
                completed_toolcall,
            )?,
            PendingTransition::NextSugar {
                summary,
                instruction,
                ..
            } => self.commit_next_sugar_pending(
                summary,
                instruction,
                close_compact,
                token_baselines,
                completed_toolcall,
            )?,
        };
        self.pending = None;
        self.control_call_ids.remove(call_id);
        Ok(Some(commit_kind))
    }

    fn commit_open_pending(
        &mut self,
        summary: String,
        mut boundary: u64,
        mut index: u64,
        token_baselines: SpineTokenBaselines,
        completed_toolcall: Option<CompletedToolCall>,
    ) -> Result<SpineCommitKind, SpineError> {
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
            .context_tokens
            .map(|_| ContextBaselineSource::ProviderAtOpen);
        let event = SpineLedgerEvent::Open {
            child: child.clone(),
            boundary,
            index,
            summary: summary.clone(),
            open_input_tokens: token_baselines.input_tokens,
            open_context_tokens: token_baselines.context_tokens,
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
                    token_baselines.input_tokens,
                    token_baselines.context_tokens,
                    open_context_source,
                )?,
            },
            &self.archive(),
        )?;
        if let Some(completed_toolcall) = completed_toolcall {
            let (toolcall_event, segments) = self.completed_toolcall_parts(&completed_toolcall)?;
            staged_parse_stack.shift(SpineToken::ToolCall { segments }, &self.archive())?;
            let events = vec![event, toolcall_event];
            self.append_committed_events_no_marker(events)?;
            self.parse_stack = staged_parse_stack;
            self.clear_completed_toolcall_anchors(&completed_toolcall);
            return Ok(SpineCommitKind::Open {
                open_request_index: usize::try_from(index).map_err(|_| {
                    SpineError::InvalidEvent("spine.open context index overflow".to_string())
                })?,
            });
        }
        let events = vec![event];
        self.append_committed_events_no_marker(events)?;
        self.parse_stack = staged_parse_stack;
        Ok(SpineCommitKind::Open {
            open_request_index: usize::try_from(index).map_err(|_| {
                SpineError::InvalidEvent("spine.open context index overflow".to_string())
            })?,
        })
    }

    fn commit_close_pending(
        &mut self,
        instruction: Option<String>,
        close_compact: Option<SpineCloseCompact>,
        token_baselines: SpineTokenBaselines,
        completed_toolcall: Option<CompletedToolCall>,
    ) -> Result<SpineCommitKind, SpineError> {
        let prepared = self.prepare_close_commit(instruction, close_compact, token_baselines)?;
        let mut staged_parse_stack = prepared.staged_parse_stack.clone();
        let mut events = vec![prepared.close_event.clone()];
        let completed_toolcall = completed_toolcall.ok_or_else(|| {
            SpineError::InvalidEvent(
                "spine.close commit requires completed toolcall evidence".to_string(),
            )
        })?;
        let toolcall_start = completed_toolcall_first_segment(&completed_toolcall)?.context_index;
        let completed_toolcall = self.remap_completed_toolcall_context_indices(
            completed_toolcall,
            prepared
                .suffix_start
                .checked_add(prepared.replacement.len())
                .ok_or_else(|| {
                    SpineError::InvalidEvent(
                        "spine.close toolcall context index overflow".to_string(),
                    )
                })?,
        )?;
        let (toolcall_event, segments) = self.completed_toolcall_parts(&completed_toolcall)?;
        staged_parse_stack.shift(SpineToken::ToolCall { segments }, &self.archive())?;
        events.push(toolcall_event);
        self.write_prepared_memory_body(&prepared.mem, &prepared.memory_body)?;
        flush_archive_writes(&prepared.archive_writes)?;
        self.commit_prepared_memory_record(&prepared.mem, &prepared.memory_body)?;
        let marker = close_commit_marker(
            self.ledger.next_event_seq,
            &prepared.mem,
            SpineCommitKindMarker::Close,
            close_event_boundary(&prepared.close_event)?,
            u64::try_from(events.len())
                .map_err(|_| SpineError::InvalidEvent("spine event count overflow".to_string()))?,
        )?;
        self.append_committed_events(events, marker)?;
        self.parse_stack = staged_parse_stack;
        self.clear_completed_toolcall_anchors(&completed_toolcall);
        Ok(SpineCommitKind::Close {
            suffix_start: prepared.suffix_start,
            replacement: prepared.replacement,
            toolcall_start,
        })
    }

    fn commit_next_sugar_pending(
        &mut self,
        summary: String,
        instruction: Option<String>,
        close_compact: Option<SpineCloseCompact>,
        token_baselines: SpineTokenBaselines,
        completed_toolcall: Option<CompletedToolCall>,
    ) -> Result<SpineCommitKind, SpineError> {
        let mut prepared =
            self.prepare_close_commit(instruction, close_compact, token_baselines)?;
        let child = prepared.staged_parse_stack.next_child_id()?;
        let open_index = prepared
            .suffix_start
            .checked_add(prepared.replacement.len())
            .ok_or_else(|| {
                SpineError::InvalidEvent("spine.next synthetic open index overflow".to_string())
            })?;
        let open_index_u64 = u64::try_from(open_index).map_err(|_| {
            SpineError::InvalidEvent("spine.next synthetic open index overflow".to_string())
        })?;
        let open_context_source = token_baselines
            .context_tokens
            .map(|_| ContextBaselineSource::ProviderAtOpen);
        let open_event = SpineLedgerEvent::Open {
            child: child.clone(),
            boundary: self.raw_len,
            index: open_index_u64,
            summary: summary.clone(),
            open_input_tokens: token_baselines.input_tokens,
            open_context_tokens: token_baselines.context_tokens,
            open_context_source,
        };
        prepared.staged_parse_stack.shift(
            SpineToken::Open {
                meta: tree_meta_with_token_baselines(
                    &self.archive(),
                    child,
                    open_index_u64,
                    summary,
                    token_baselines.input_tokens,
                    token_baselines.context_tokens,
                    open_context_source,
                )?,
            },
            &self.archive(),
        )?;
        let mut events = vec![prepared.close_event.clone(), open_event];
        let completed_toolcall = completed_toolcall.ok_or_else(|| {
            SpineError::InvalidEvent(
                "spine.next commit requires completed toolcall evidence".to_string(),
            )
        })?;
        let toolcall_start = completed_toolcall_first_segment(&completed_toolcall)?.context_index;
        let completed_toolcall =
            self.remap_completed_toolcall_context_indices(completed_toolcall, open_index)?;
        let (toolcall_event, segments) = self.completed_toolcall_parts(&completed_toolcall)?;
        prepared
            .staged_parse_stack
            .shift(SpineToken::ToolCall { segments }, &self.archive())?;
        events.push(toolcall_event);
        self.write_prepared_memory_body(&prepared.mem, &prepared.memory_body)?;
        flush_archive_writes(&prepared.archive_writes)?;
        self.commit_prepared_memory_record(&prepared.mem, &prepared.memory_body)?;
        let marker = close_commit_marker(
            self.ledger.next_event_seq,
            &prepared.mem,
            SpineCommitKindMarker::CloseThenOpen,
            close_event_boundary(&prepared.close_event)?,
            u64::try_from(events.len())
                .map_err(|_| SpineError::InvalidEvent("spine event count overflow".to_string()))?,
        )?;
        self.append_committed_events(events, marker)?;
        self.parse_stack = prepared.staged_parse_stack;
        self.clear_completed_toolcall_anchors(&completed_toolcall);
        Ok(SpineCommitKind::CloseThenOpen {
            suffix_start: prepared.suffix_start,
            replacement: prepared.replacement,
            toolcall_start,
            open_index,
        })
    }

    fn prepare_close_commit(
        &self,
        instruction: Option<String>,
        close_compact: Option<SpineCloseCompact>,
        token_baselines: SpineTokenBaselines,
    ) -> Result<PreparedCloseCommit, SpineError> {
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
            instruction,
            close_input_tokens: token_baselines.input_tokens,
            close_context_tokens: token_baselines.context_tokens,
        };
        let close_compact = close_compact.ok_or_else(|| {
            SpineError::CompactFailure(format!(
                "spine.close requires a completed suffix compact for node {}",
                open_meta.id
            ))
        })?;
        let seq = self.ledger.next_event_seq;
        if close_compact.source_context_range.start != suffix_start {
            return Err(SpineError::CompactFailure(format!(
                "spine.close compact context range starts at {}, expected suffix start {suffix_start} for node {}",
                close_compact.source_context_range.start, open_meta.id
            )));
        }
        let expected_raw_start = self.open_raw_start(&open_meta.id)?;
        if close_compact.source_raw_range.start != expected_raw_start {
            return Err(SpineError::CompactFailure(format!(
                "spine.close compact raw range starts at {}, expected raw start {expected_raw_start} for node {}",
                close_compact.source_raw_range.start, open_meta.id
            )));
        }
        if close_compact.source_raw_range.end > self.raw_len {
            return Err(SpineError::CompactFailure(format!(
                "spine.close compact raw range end {} exceeds raw_len {} for node {}",
                close_compact.source_raw_range.end, self.raw_len, open_meta.id
            )));
        }
        let body = close_compact.body.clone();
        let mem = self.stage_close_mem(&open_meta, &close_compact, token_baselines)?;
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
            mem.open_context_source,
            mem.memory_output_tokens,
        );
        let staged_archive = SpineArchive::staged_with_memory_body(
            self.store.root.clone(),
            mem.compact_id.clone(),
            body.clone(),
        );
        let mut staged_parse_stack = self.parse_stack.clone();
        staged_parse_stack.shift(SpineToken::Close { memory }, &staged_archive)?;
        let archive_writes = staged_archive.staged_writes();
        let replacement = vec![memory_response_item(&body)];
        Ok(PreparedCloseCommit {
            suffix_start,
            replacement,
            mem,
            memory_body: body,
            archive_writes,
            close_event,
            staged_parse_stack,
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
            PendingTransition::Close { instruction, .. } => {
                let open_meta = self.current_close_open_meta()?;
                SpinePendingCommit::Close {
                    action: SpinePendingCloseAction::Close,
                    node: open_meta.id.clone(),
                    suffix_start: open_meta.index,
                    instruction: instruction.clone(),
                    next_summary: None,
                }
            }
            PendingTransition::NextSugar {
                summary,
                instruction,
                ..
            } => {
                let open_meta = self.current_close_open_meta()?;
                SpinePendingCommit::Close {
                    action: SpinePendingCloseAction::Next,
                    node: open_meta.id.clone(),
                    suffix_start: open_meta.index,
                    instruction: instruction.clone(),
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
        let [Symbol::SpineTreeNodes(nodes)] = suffix else {
            return Err(SpineError::InvalidEvent(format!(
                "spine.close source plan expected one live node list after current Open, found {suffix:?}"
            )));
        };
        Ok(nodes)
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
        self.root_compact_impl(
            body,
            raw_items,
            SpineRootCompactTokenMetadata::default(),
            None,
        )
        .map(|result| result.materialized)
    }

    pub(crate) fn root_compact_with_checkpoint(
        &mut self,
        rollout_path: &Path,
        body: String,
        raw_items: &[Option<ResponseItem>],
        token_metadata: SpineRootCompactTokenMetadata,
    ) -> Result<SpineRootCompactResult, SpineError> {
        self.root_compact_impl(body, raw_items, token_metadata, Some(rollout_path))
    }

    fn root_compact_impl(
        &mut self,
        body: String,
        raw_items: &[Option<ResponseItem>],
        token_metadata: SpineRootCompactTokenMetadata,
        checkpoint_rollout_path: Option<&Path>,
    ) -> Result<SpineRootCompactResult, SpineError> {
        let prepared = self.prepare_root_compact_commit(
            body,
            raw_items,
            token_metadata,
            checkpoint_rollout_path,
        )?;
        self.write_prepared_memory_body(&prepared.mem, &prepared.memory_body)?;
        self.commit_prepared_memory_record(&prepared.mem, &prepared.memory_body)?;
        if let Some(checkpoint) = prepared.compact_checkpoint.as_ref() {
            self.store.append_compact_checkpoint(checkpoint)?;
        }
        let marker = root_compact_commit_marker(self.ledger.next_event_seq, &prepared.mem)?;
        self.append_committed_events(vec![prepared.root_compact_event], marker)?;
        self.parse_stack = prepared.staged_parse_stack;
        self.pending = None;
        Ok(prepared.result)
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
            mem.open_context_source,
            mem.memory_output_tokens,
        );

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
        let staged_memory_body = Some((compact_id.as_str(), body.as_str()));
        let next_open_index = render_parse_stack_to_context_with_memory_body(
            &probe_parse_stack,
            raw_items,
            staged_memory_body,
        )?
        .len();

        let mut staged_parse_stack = self.parse_stack.clone();
        staged_parse_stack.shift(
            SpineToken::Compact {
                memory,
                next_open_index,
                next_open_input_tokens: token_metadata.next_open_input_tokens,
                next_open_context_tokens: token_metadata.next_open_context_tokens,
            },
            &self.archive(),
        )?;
        let materialized = render_parse_stack_to_context_with_memory_body(
            &staged_parse_stack,
            raw_items,
            staged_memory_body,
        )?;
        let current_open_index = staged_parse_stack.current_open_meta()?.index;
        if current_open_index != materialized.len() {
            return Err(SpineError::Invariant(format!(
                "spine root compact open index {current_open_index} does not match materialized history length {}",
                materialized.len()
            )));
        }
        let next_open_index = u64::try_from(next_open_index)
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
            next_open_index,
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
            staged_parse_stack,
        })
    }

    fn stage_close_mem(
        &self,
        open_meta: &TreeMeta,
        close_compact: &SpineCloseCompact,
        token_baselines: SpineTokenBaselines,
    ) -> Result<MemRecord, SpineError> {
        let node_id = open_meta.id.clone();
        let raw_start = close_compact.source_raw_range.start;
        let end = close_compact.source_raw_range.end;
        let compact_id = format!(
            "mem-{}-{}-{}",
            node_id.as_path().replace('.', "-"),
            raw_start,
            end
        );
        let body_path = format!("{BODY_DIR}/{compact_id}.md");
        let open_context_baseline = self.open_context_baseline_for(open_meta);
        let mem = MemRecord {
            compact_id,
            kind: MemKind::Suffix,
            node: node_id,
            raw_start,
            raw_end: end,
            context_start: close_compact.source_context_range.start,
            context_end: close_compact.source_context_range.end,
            raw_live_hash: None,
            open_input_tokens: open_context_baseline
                .and_then(|baseline| baseline.input_tokens)
                .or(open_meta.open_input_tokens),
            close_input_tokens: token_baselines.input_tokens,
            open_context_tokens: open_context_baseline.map(|baseline| baseline.context_tokens),
            close_context_tokens: token_baselines.context_tokens,
            open_context_source: open_context_baseline.map(|baseline| baseline.source),
            memory_output_tokens: close_compact.memory_output_tokens,
            body_path,
            body_hash: sha1_hex(close_compact.body.as_bytes()),
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
        render_parse_stack_to_context(&self.parse_stack, raw_items)
    }

    pub(crate) fn validate_raw_coverage(
        &self,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<(), SpineError> {
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
                SpineLedgerEvent::Init { .. } => {}
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

fn protocol_context_baseline_source(
    source: ContextBaselineSource,
) -> codex_protocol::spine_tree::SpineNodeContextBaselineSource {
    match source {
        ContextBaselineSource::ProviderAtOpen => {
            codex_protocol::spine_tree::SpineNodeContextBaselineSource::ProviderAtOpen
        }
        ContextBaselineSource::RootCompactHandoff => {
            codex_protocol::spine_tree::SpineNodeContextBaselineSource::RootCompactHandoff
        }
        ContextBaselineSource::EstimatedFromLiveSuffix => {
            codex_protocol::spine_tree::SpineNodeContextBaselineSource::EstimatedFromLiveSuffix
        }
        ContextBaselineSource::CheckpointReplay => {
            codex_protocol::spine_tree::SpineNodeContextBaselineSource::CheckpointReplay
        }
    }
}

fn next_event_seq_from(events: &[LoggedSpineLedgerEvent]) -> Result<u64, SpineError> {
    events
        .iter()
        .map(|event| event.seq)
        .max()
        .map(|seq| {
            seq.checked_add(1)
                .ok_or_else(|| SpineError::InvalidEvent("spine event seq overflow".to_string()))
        })
        .transpose()
        .map(|seq| seq.unwrap_or(0))
}

fn next_pressure_seq_from(events: &[LoggedPressureEvent]) -> Result<u64, SpineError> {
    events
        .iter()
        .map(|event| event.pressure_seq)
        .max()
        .map(|seq| {
            seq.checked_add(1)
                .ok_or_else(|| SpineError::InvalidEvent("spine pressure seq overflow".to_string()))
        })
        .transpose()
        .map(|seq| seq.unwrap_or(0))
}

fn replay_pressure_baselines(
    parse_stack: &ParseStack,
    events: &[LoggedPressureEvent],
    raw_live: &[bool],
    current_structural_seq: u64,
    pressure_seq_watermark: Option<u64>,
    limit_to_pressure_watermark: bool,
) -> BTreeMap<NodeId, OpenContextBaseline> {
    let live_open_nodes = parse_stack
        .live_open_metas()
        .into_iter()
        .map(|meta| meta.id.clone())
        .collect::<BTreeSet<_>>();
    let mut baselines = BTreeMap::new();
    for event in events {
        if limit_to_pressure_watermark {
            let Some(watermark) = pressure_seq_watermark else {
                continue;
            };
            if event.pressure_seq > watermark {
                continue;
            }
        }
        if !limit_to_pressure_watermark
            && pressure_seq_watermark.is_some_and(|watermark| event.pressure_seq > watermark)
        {
            continue;
        }
        if !event.allowed_by(raw_live) {
            continue;
        }
        match &event.event {
            PressureEvent::OpenContextBaseline {
                node,
                observed_structural_seq,
                context_tokens,
                input_tokens,
                source,
                ..
            } => {
                if *observed_structural_seq > current_structural_seq
                    || !live_open_nodes.contains(node)
                    || *context_tokens < 0
                {
                    continue;
                }
                baselines.insert(
                    node.clone(),
                    OpenContextBaseline {
                        context_tokens: *context_tokens,
                        input_tokens: *input_tokens,
                        source: *source,
                    },
                );
            }
        }
    }
    baselines
}

fn open_structural_seq_for(events: &[LoggedSpineLedgerEvent], node_id: &NodeId) -> Option<u64> {
    events.iter().find_map(|event| match &event.event {
        SpineLedgerEvent::Open { child, .. } if child == node_id => Some(event.seq),
        SpineLedgerEvent::RootCompact { node, .. }
            if node
                .0
                .first()
                .and_then(|root| root.checked_add(1))
                .map(NodeId::root_epoch)
                .map(|next_root| next_root.child(1) == *node_id)
                .unwrap_or(false) =>
        {
            Some(event.seq)
        }
        _ => None,
    })
}

fn replay_from_events(
    archive: &SpineArchive,
    events: &[LoggedSpineLedgerEvent],
    mems: &[MemRecord],
    raw_live: &[bool],
    replay_event_seqs: &MarkerReplayEventSeqs,
    initial: Option<&ParseStack>,
    min_seq: Option<u64>,
) -> Result<ParseStack, SpineError> {
    let raw_mask = RawMask::new(raw_live);
    let Some(initial) = initial else {
        let events = events
            .iter()
            .filter(|event| min_seq.is_none_or(|min_seq| event.seq >= min_seq))
            .cloned()
            .collect::<Vec<_>>();
        return parse_stack_from_events_with_forced_events(
            &events,
            archive,
            mems,
            raw_mask,
            &replay_event_seqs.forced,
            &replay_event_seqs.marker_structural,
        );
    };
    let mem_map = mems
        .iter()
        .cloned()
        .map(|mem| (mem.compact_id.clone(), mem))
        .collect::<BTreeMap<_, _>>();
    let mut parse_stack = initial.clone();
    for event in events
        .iter()
        .filter(|event| min_seq.is_none_or(|min_seq| event.seq >= min_seq))
    {
        if replay_event_seqs.forced.contains(&event.seq) {
            parse_stack.shift(event_to_token(event, archive, &mem_map, raw_mask)?, archive)?;
            continue;
        }
        if replay_event_seqs.marker_structural.contains(&event.seq)
            || !event.allowed_by(raw_mask)?
        {
            continue;
        }
        parse_stack.shift(event_to_token(event, archive, &mem_map, raw_mask)?, archive)?;
    }
    Ok(parse_stack)
}

struct MarkerReplayEventSeqs {
    forced: BTreeSet<u64>,
    marker_structural: BTreeSet<u64>,
}

fn replay_event_seqs_from_markers(
    events: &[LoggedSpineLedgerEvent],
    markers: &[SpineCommitMarker],
    mems: &[MemRecord],
    raw_mask: RawMask<'_>,
    min_seq: Option<u64>,
    max_seq: Option<u64>,
    fail_on_unproved_raw_backed: bool,
) -> Result<MarkerReplayEventSeqs, SpineError> {
    let mems_by_id = mems
        .iter()
        .map(|mem| (mem.compact_id.as_str(), mem))
        .collect::<BTreeMap<_, _>>();
    let events_by_seq = events
        .iter()
        .map(|event| (event.seq, event))
        .collect::<BTreeMap<_, _>>();
    let mut forced = BTreeSet::new();
    let mut marker_structural = BTreeSet::new();
    for marker in markers {
        if !marker_in_replay_range(marker, min_seq, max_seq) {
            continue;
        }
        let structural_event_seqs = commit_marker_structural_event_seqs(marker)?;
        marker_structural.extend(structural_event_seqs.iter().copied());
        if commit_marker_transaction_live_for_replay(
            marker,
            &structural_event_seqs,
            &events_by_seq,
            &mems_by_id,
            raw_mask,
            fail_on_unproved_raw_backed,
        )? {
            forced.extend(structural_event_seqs);
        }
    }
    Ok(MarkerReplayEventSeqs {
        forced,
        marker_structural,
    })
}

fn commit_marker_transaction_live_for_replay(
    marker: &SpineCommitMarker,
    structural_event_seqs: &BTreeSet<u64>,
    events_by_seq: &BTreeMap<u64, &LoggedSpineLedgerEvent>,
    mems_by_id: &BTreeMap<&str, &MemRecord>,
    raw_mask: RawMask<'_>,
    fail_on_unproved_raw_backed: bool,
) -> Result<bool, SpineError> {
    for memory in &marker.memory_refs {
        let Some(mem) = mems_by_id.get(memory.compact_id.as_str()) else {
            return Ok(false);
        };
        if !mem.allowed_by(raw_mask)? {
            return Ok(false);
        }
    }
    for seq in marker.token_seq_start..marker.token_seq_end {
        if structural_event_seqs.contains(&seq) {
            continue;
        }
        let Some(event) = events_by_seq.get(&seq) else {
            return Ok(false);
        };
        if !event.allowed_by(raw_mask)? {
            if !fail_on_unproved_raw_backed {
                return Ok(false);
            }
            return Err(SpineError::InvalidStore(format!(
                "Spine commit marker {} raw-backed event at token_seq {} is not proved by live raw state",
                marker.op_id, seq
            )));
        }
    }
    Ok(true)
}

fn marker_in_replay_range(
    marker: &SpineCommitMarker,
    min_seq: Option<u64>,
    max_seq: Option<u64>,
) -> bool {
    min_seq.is_none_or(|min_seq| marker.token_seq_start >= min_seq)
        && max_seq.is_none_or(|max_seq| marker.token_seq_end <= max_seq)
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
            } => collect_source_plan_entry_from_response_item(
                *raw_ordinal,
                visible_ref.context_index,
                *from_user,
                raw_context_items,
                &mut entries,
            )?,
            VisibleItemSource::ToolCallSegment { raw_ordinal, kind } => {
                let _ = kind;
                collect_source_plan_entry_from_response_item(
                    *raw_ordinal,
                    visible_ref.context_index,
                    false,
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
        },
    });
    Ok(())
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
        && existing.open_context_source == expected.open_context_source
        && existing.memory_output_tokens == expected.memory_output_tokens
        && existing.body_path == expected.body_path
        && existing.body_hash == expected.body_hash
}

pub(crate) fn is_user_message(item: &ResponseItem) -> bool {
    matches!(item, ResponseItem::Message { role, .. } if role == "user")
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
