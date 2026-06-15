use crate::spine::SpineError;
use crate::spine::io::hash_raw_live;
use crate::spine::io::hash_raw_live_prefix_all_true;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fmt;
use std::ops::Range;
use std::path::PathBuf;

pub(super) const COMMIT_MARKER_VERSION: u32 = 1;
pub(crate) const TOOL_RESPONSE_TRIM_THRESHOLD_BYTES: i64 = 500;
pub(crate) const TOOL_RESULT_CLEARED_MESSAGE: &str = "[Old tool result content cleared]";

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct NodeId(pub(super) Vec<u32>);

impl NodeId {
    pub(super) fn root_epoch(index: u32) -> Self {
        Self(vec![index])
    }

    pub(super) fn child(&self, index: u32) -> Self {
        let mut path = self.0.clone();
        path.push(index);
        Self(path)
    }

    pub(super) fn parent(&self) -> Option<Self> {
        (self.0.len() > 1).then(|| Self(self.0[..self.0.len() - 1].to_vec()))
    }

    pub(super) fn is_root_epoch(&self) -> bool {
        self.0.len() == 1
    }

    pub(super) fn as_path(&self) -> String {
        self.0
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(".")
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.as_path())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum NodeStatus {
    Live,
    Opened,
    Closed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ContextBaselineSource {
    ProviderAtOpen,
    RootCompactHandoff,
    EstimatedFromLiveSuffix,
    CheckpointReplay,
}

/// Durable sidecar event ledger, replayed into SpineToken for ParseStack.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum SpineLedgerEvent {
    Init {
        raw_start: u64,
    },
    Msg {
        raw_ordinal: u64,
        context_index: u64,
        from_user: bool,
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
        instruction: Option<String>,
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
}

/// Append-only Spine ledger event with a monotonic sidecar sequence number.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct LoggedSpineLedgerEvent {
    pub(super) seq: u64,
    #[serde(flatten)]
    pub(super) event: SpineLedgerEvent,
}

impl std::ops::Deref for LoggedSpineLedgerEvent {
    type Target = SpineLedgerEvent;

    fn deref(&self) -> &Self::Target {
        &self.event
    }
}

impl LoggedPressureEvent {
    pub(super) fn allowed_by(&self, raw_live: &[bool]) -> bool {
        match &self.event {
            PressureEvent::OpenContextBaseline {
                observed_raw_ordinal,
                observed_raw_live_hash,
                ..
            } => {
                let Ok(end) = usize::try_from(*observed_raw_ordinal) else {
                    return false;
                };
                if end > raw_live.len() {
                    return false;
                }
                observed_raw_live_hash
                    .as_deref()
                    .map(|hash| hash_raw_live(&raw_live[..end]) == hash)
                    .unwrap_or(true)
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum PressureEvent {
    OpenContextBaseline {
        node: NodeId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        open_structural_seq: Option<u64>,
        observed_structural_seq: u64,
        observed_raw_ordinal: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        observed_raw_live_hash: Option<String>,
        observed_context_index: usize,
        context_tokens: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_tokens: Option<i64>,
        source: ContextBaselineSource,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        estimated_live_suffix_tokens: Option<i64>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct LoggedPressureEvent {
    pub(super) pressure_seq: u64,
    #[serde(flatten)]
    pub(super) event: PressureEvent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum TrimResponseKind {
    FunctionCallOutput,
    CustomToolCallOutput,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum TrimEvent {
    Candidate {
        trim_id: String,
        toolcall_seq: u64,
        raw_ordinal: u64,
        context_index: usize,
        call_id: String,
        response_kind: TrimResponseKind,
        original_visible_size: i64,
    },
    Cleared {
        trim_id: String,
        raw_boundary: u64,
        raw_live_hash: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct LoggedTrimEvent {
    pub(super) trim_seq: u64,
    #[serde(flatten)]
    pub(super) event: TrimEvent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TrimTarget {
    pub(super) trim_id: String,
    pub(super) toolcall_seq: u64,
    pub(super) raw_ordinal: u64,
    pub(super) context_index: usize,
    pub(super) call_id: String,
    pub(super) response_kind: TrimResponseKind,
    pub(super) original_visible_size: i64,
    pub(super) cleared: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct TrimProjection {
    pub(super) targets_by_id: BTreeMap<String, TrimTarget>,
    pub(super) trim_id_by_raw_ordinal: BTreeMap<u64, String>,
}

impl TrimProjection {
    pub(super) fn target_for_raw_ordinal(&self, raw_ordinal: u64) -> Option<&TrimTarget> {
        let trim_id = self.trim_id_by_raw_ordinal.get(&raw_ordinal)?;
        self.targets_by_id.get(trim_id)
    }

    pub(super) fn target(&self, trim_id: &str) -> Option<&TrimTarget> {
        self.targets_by_id.get(trim_id)
    }

    pub(super) fn insert_candidate(&mut self, target: TrimTarget) {
        self.trim_id_by_raw_ordinal
            .insert(target.raw_ordinal, target.trim_id.clone());
        self.targets_by_id.insert(target.trim_id.clone(), target);
    }

    pub(super) fn mark_cleared(&mut self, trim_id: &str) {
        if let Some(target) = self.targets_by_id.get_mut(trim_id) {
            target.cleared = true;
        }
    }

    pub(super) fn contains_toolcall_raw_target(&self, toolcall_seq: u64, raw_ordinal: u64) -> bool {
        self.targets_by_id
            .values()
            .any(|target| target.toolcall_seq == toolcall_seq && target.raw_ordinal == raw_ordinal)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum MemKind {
    Suffix,
    RootEpoch,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct MemRecord {
    pub(super) compact_id: String,
    pub(super) kind: MemKind,
    pub(super) node: NodeId,
    pub(super) raw_start: u64,
    pub(super) raw_end: u64,
    pub(super) context_start: usize,
    pub(super) context_end: usize,
    #[serde(default)]
    pub(super) raw_live_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) open_input_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) close_input_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) open_context_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) close_context_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) open_context_source: Option<ContextBaselineSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) memory_output_tokens: Option<i64>,
    pub(super) body_path: String,
    pub(super) body_hash: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum SpineCommitKindMarker {
    Close,
    CloseThenOpen,
    RootCompact,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct SpineCommitMemoryRef {
    pub(super) compact_id: String,
    pub(super) kind: MemKind,
    pub(super) node: NodeId,
    pub(super) raw_start: u64,
    pub(super) raw_end: u64,
    pub(super) context_start: usize,
    pub(super) context_end: usize,
    #[serde(default)]
    pub(super) raw_live_hash: Option<String>,
    pub(super) body_path: String,
    pub(super) body_hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct SpineCommitMarker {
    pub(super) version: u32,
    pub(super) op_id: String,
    pub(super) kind: SpineCommitKindMarker,
    pub(super) token_seq_start: u64,
    pub(super) token_seq_end: u64,
    pub(super) raw_boundary: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) raw_live_hash: Option<String>,
    pub(super) memory_refs: Vec<SpineCommitMemoryRef>,
}

pub(super) fn commit_marker_structural_event_seqs(
    marker: &SpineCommitMarker,
) -> Result<BTreeSet<u64>, SpineError> {
    let mut seqs = BTreeSet::new();
    match marker.kind {
        SpineCommitKindMarker::Close => {
            seqs.insert(marker.token_seq_start);
        }
        SpineCommitKindMarker::CloseThenOpen => {
            seqs.insert(marker.token_seq_start);
            seqs.insert(marker.token_seq_start.checked_add(1).ok_or_else(|| {
                SpineError::InvalidEvent("Spine commit marker token seq overflow".to_string())
            })?);
        }
        SpineCommitKindMarker::RootCompact => {
            seqs.insert(marker.token_seq_start);
        }
    }
    Ok(seqs)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct TreeMeta {
    pub(super) id: NodeId,
    pub(super) index: usize,
    pub(super) summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) open_input_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) open_context_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) open_context_source: Option<ContextBaselineSource>,
    pub(super) node_dir: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum SegRef {
    ResponseItem {
        raw_ordinal: u64,
        context_index: usize,
    },
    Memory {
        memory_id: String,
        body_path: PathBuf,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ToolCallSegmentKind {
    Request,
    Response,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct ToolCallEventSegment {
    pub(super) kind: ToolCallSegmentKind,
    pub(super) raw_ordinal: u64,
    pub(super) context_index: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct ToolCallSegment {
    pub(super) kind: ToolCallSegmentKind,
    pub(super) seg: SegRef,
}

impl SegRef {
    #[cfg(test)]
    pub(super) fn from_memory_ref(memory: &MemoryRef) -> Self {
        Self::Memory {
            memory_id: memory.compact_id.clone(),
            body_path: memory.body_path.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct MemoryRef {
    pub(super) compact_id: String,
    pub(super) node_id: NodeId,
    pub(super) body_path: PathBuf,
    pub(super) body_hash: String,
    pub(super) source_raw_range: Range<u64>,
    pub(super) source_context_range: Range<usize>,
    pub(super) source_token_seq: Range<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) open_input_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) close_input_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) open_context_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) close_context_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) open_context_source: Option<ContextBaselineSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) memory_output_tokens: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum SpineToken {
    Init {
        meta: TreeMeta,
    },
    End,
    Open {
        meta: TreeMeta,
    },
    Close {
        memory: MemoryRef,
    },
    Compact {
        memory: MemoryRef,
        next_open_index: usize,
        next_open_input_tokens: Option<i64>,
        next_open_context_tokens: Option<i64>,
    },
    Msg {
        seg: SegRef,
        from_user: bool,
    },
    ToolCall {
        segments: Vec<ToolCallSegment>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum ControlSymbol {
    Init(TreeMeta),
    End,
    Open(TreeMeta),
    Close(MemoryRef),
    Compact(MemoryRef, usize, Option<i64>, Option<i64>),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum Symbol {
    Control(ControlSymbol),
    SpineTreeNode(SpineTreeNode),
    SpineTreeNodes(Vec<SpineTreeNode>),
    RootEpoches(Vec<RootEpoch>),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum SpineTreeNode {
    MsgAsLeafNode {
        msg: SegRef,
        from_user: bool,
    },
    ToolCallAsLeafNode {
        segments: Vec<ToolCallSegment>,
    },
    SpineTree {
        memory: MemoryRef,
        meta: TreeMeta,
        children: Vec<SpineTreeNode>,
        memory_path: PathBuf,
        trajs_path: PathBuf,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct RootEpoch {
    pub(super) memory: MemoryRef,
}

#[derive(Clone, Copy)]
pub(super) struct RawMask<'a> {
    live: Option<&'a [bool]>,
}

impl<'a> RawMask<'a> {
    pub(super) fn new(live: &'a [bool]) -> Self {
        Self { live: Some(live) }
    }

    fn boundary_live(self, boundary: u64) -> Result<bool, SpineError> {
        let Some(live) = self.live else {
            return Ok(true);
        };
        if boundary == 0 {
            return Ok(true);
        }
        let index = usize::try_from(boundary - 1)
            .map_err(|_| SpineError::InvalidEvent("raw boundary overflow".to_string()))?;
        Ok(live.get(index).copied().unwrap_or(false))
    }

    fn raw_index_live(self, index: u64) -> Result<bool, SpineError> {
        let Some(live) = self.live else {
            return Ok(true);
        };
        let index = usize::try_from(index)
            .map_err(|_| SpineError::InvalidEvent("raw index overflow".to_string()))?;
        Ok(live.get(index).copied().unwrap_or(false))
    }

    fn span_live(self, start: u64, end: u64) -> Result<bool, SpineError> {
        let Some(live) = self.live else {
            return Ok(true);
        };
        let start = usize::try_from(start)
            .map_err(|_| SpineError::InvalidEvent("raw start overflow".to_string()))?;
        let end = usize::try_from(end)
            .map_err(|_| SpineError::InvalidEvent("raw end overflow".to_string()))?;
        if end > live.len() || start > end {
            return Ok(false);
        }
        Ok(live[start..end].iter().all(|item| *item))
    }

    fn prefix_hash_matches(self, end: u64, expected: &str) -> Result<bool, SpineError> {
        let end = usize::try_from(end)
            .map_err(|_| SpineError::InvalidEvent("raw end overflow".to_string()))?;
        let Some(live) = self.live else {
            return Ok(hash_raw_live_prefix_all_true(end) == expected);
        };
        if end > live.len() {
            return Ok(false);
        }
        Ok(hash_raw_live(&live[..end]) == expected)
    }
}

impl LoggedSpineLedgerEvent {
    pub(super) fn allowed_by(&self, raw_mask: RawMask<'_>) -> Result<bool, SpineError> {
        self.event.allowed_by(raw_mask)
    }
}

impl LoggedTrimEvent {
    pub(super) fn allowed_by(&self, raw_mask: RawMask<'_>) -> Result<bool, SpineError> {
        self.event.allowed_by(raw_mask)
    }
}

impl TrimEvent {
    pub(super) fn allowed_by(&self, raw_mask: RawMask<'_>) -> Result<bool, SpineError> {
        match self {
            TrimEvent::Candidate { raw_ordinal, .. } => raw_mask.raw_index_live(*raw_ordinal),
            TrimEvent::Cleared {
                raw_boundary,
                raw_live_hash,
                ..
            } => raw_mask.prefix_hash_matches(*raw_boundary, raw_live_hash),
        }
    }
}

impl SpineLedgerEvent {
    pub(super) fn allowed_by(&self, raw_mask: RawMask<'_>) -> Result<bool, SpineError> {
        match self {
            SpineLedgerEvent::Init { .. } => Ok(true),
            SpineLedgerEvent::Msg { raw_ordinal, .. } => raw_mask.raw_index_live(*raw_ordinal),
            SpineLedgerEvent::ToolCall { segments } => {
                for segment in segments {
                    if !raw_mask.raw_index_live(segment.raw_ordinal)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            SpineLedgerEvent::Open {
                child,
                summary,
                boundary,
                ..
            } => {
                if summary == "root" && child.parent().is_some_and(|parent| parent.is_root_epoch())
                {
                    return Ok(true);
                }
                raw_mask.raw_index_live(*boundary)
            }
            SpineLedgerEvent::Close { boundary, .. } => raw_mask.boundary_live(*boundary),
            SpineLedgerEvent::RootCompact {
                boundary,
                raw_live_hash,
                ..
            } => raw_mask.prefix_hash_matches(*boundary, raw_live_hash),
        }
    }
}

impl MemRecord {
    pub(super) fn allowed_by(&self, raw_mask: RawMask<'_>) -> Result<bool, SpineError> {
        match self.kind {
            MemKind::Suffix => raw_mask.span_live(self.raw_start, self.raw_end),
            MemKind::RootEpoch => self
                .raw_live_hash
                .as_deref()
                .map(|hash| raw_mask.prefix_hash_matches(self.raw_end, hash))
                .unwrap_or(Ok(false)),
        }
    }
}
