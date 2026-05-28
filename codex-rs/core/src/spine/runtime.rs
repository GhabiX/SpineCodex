use codex_protocol::models::ResponseItem;
use codex_protocol::spine_tree::SpineTreeUpdateEvent;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::ops::Range;
use std::path::Path;
use thiserror::Error;

use crate::spine::archive::SpineArchive;
use crate::spine::archive::memory_ref;
use crate::spine::archive::tree_meta_with_open_input_tokens;
use crate::spine::checkpoint::SpineCheckpoint;
use crate::spine::checkpoint::build_checkpoint;
use crate::spine::checkpoint::validate_checkpoint;
use crate::spine::io::hash_raw_live;
use crate::spine::io::sha1_hex;
#[cfg(test)]
use crate::spine::model::ControlSymbol;
use crate::spine::model::KEvent;
use crate::spine::model::LoggedKEvent;
use crate::spine::model::MemKind;
use crate::spine::model::MemRecord;
use crate::spine::model::NodeId;
use crate::spine::model::RawMask;
use crate::spine::model::SegRef;
use crate::spine::model::SpineToken;
#[cfg(test)]
use crate::spine::model::SpineTreeNode;
#[cfg(test)]
use crate::spine::model::Symbol;
use crate::spine::model::TreeMeta;
use crate::spine::parse_stack::ParseStack;
use crate::spine::parse_stack::event_to_token;
use crate::spine::parse_stack::parse_stack_from_events;
#[cfg(test)]
use crate::spine::parse_stack::parse_stack_msg_leaf_count;
use crate::spine::render::memory_response_item;
use crate::spine::render::render_parse_stack_to_context;
use crate::spine::store::SpineStore;

pub(crate) const SPINE_NAMESPACE: &str = "spine";
pub(crate) const SPINE_TOOL_TREE: &str = "tree";
pub(crate) const SPINE_TOOL_OPEN: &str = "open";
pub(crate) const SPINE_TOOL_CLOSE: &str = "close";

#[derive(Clone, Debug)]
pub(crate) struct SpineRuntime {
    store: SpineStore,
    parse_stack: ParseStack,
    raw_len: u64,
    raw_live: Vec<bool>,
    // Turn-local Spine control transaction state. Committed open/close effects
    // are represented by KEvents and ParseStack tokens; these maps are empty on
    // resume/rollback by design and are not part of h(PS).
    open_requests: BTreeMap<String, OpenRequestAnchor>,
    control_call_ids: BTreeSet<String>,
    pending: Option<PendingTransition>,
}

#[derive(Clone, Debug)]
struct OpenRequestAnchor {
    raw_ordinal: u64,
    context_index: u64,
}

#[derive(Clone, Debug)]
struct PendingTransition {
    call_id: String,
    op: SpineOp,
    summary: Option<String>,
    boundary: Option<u64>,
    index: Option<u64>,
    instruction: Option<String>,
}

#[derive(Clone, Debug)]
struct PendingMsg {
    raw_ordinal: u64,
    context_index: u64,
    from_user: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SpineOp {
    Open,
    Close,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SpineCommitKind {
    Open {
        open_request_index: usize,
    },
    Close {
        suffix_start: usize,
        replacement: Vec<ResponseItem>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SpinePendingCommit {
    Open,
    Close {
        node: NodeId,
        suffix_start: usize,
        instruction: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpineCloseCompact {
    pub(crate) body: String,
    pub(crate) source_context_range: Range<usize>,
    pub(crate) memory_output_tokens: Option<i64>,
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

    fn invalid_error(&self) -> Option<SpineError> {
        self.invalid
            .as_ref()
            .map(|reason| SpineError::InvalidStore(format!("spine runtime is invalid: {reason}")))
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
            store.append_event(&KEvent::Init { raw_start: 0 })?;
            store.append_event(&KEvent::Open {
                child: NodeId::root_epoch(1).child(1),
                boundary: raw_len,
                index: raw_len,
                summary: "root".to_string(),
                open_input_tokens: None,
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
        Self::load_with_raw_live(store, raw_live)
    }

    fn load_with_raw_live_and_event_limit(
        store: SpineStore,
        raw_live: Vec<bool>,
        event_limit: Option<u64>,
    ) -> Result<Self, SpineError> {
        let events = store.events()?;
        let mems = store.mems()?;
        let events = if let Some(limit) = event_limit {
            events
                .into_iter()
                .filter(|event| event.seq < limit)
                .collect::<Vec<_>>()
        } else {
            events
        };
        let archive = SpineArchive::new(store.root.clone());
        let parse_stack = replay_from_events(&archive, &events, &mems, &raw_live, None, None)?;
        Ok(Self {
            store,
            parse_stack,
            raw_len: u64::try_from(raw_live.len())
                .map_err(|_| SpineError::InvalidEvent("raw item count overflow".to_string()))?,
            raw_live,
            open_requests: BTreeMap::new(),
            control_call_ids: BTreeSet::new(),
            pending: None,
        })
    }

    fn load_with_rollback_checkpoint(
        store: SpineStore,
        raw_live: Vec<bool>,
        checkpoint: &SpineCheckpoint,
    ) -> Result<Self, SpineError> {
        let events = store.events()?;
        let mems = store.mems()?;
        let archive = SpineArchive::new(store.root.clone());
        let raw_ordinal = usize::try_from(checkpoint.raw_ordinal)
            .map_err(|_| SpineError::InvalidEvent("checkpoint raw ordinal overflow".to_string()))?;
        let prefix_live = &raw_live[..raw_ordinal.min(raw_live.len())];
        let prefix_mask = RawMask::new(prefix_live);
        let prefix_events = events
            .iter()
            .filter(|event| event.seq < checkpoint.token_seq)
            .cloned()
            .collect::<Vec<_>>();
        let prefix_ps = parse_stack_from_events(&prefix_events, &archive, &mems, prefix_mask)?;
        if prefix_ps != checkpoint.parse_stack {
            return Err(SpineError::InvalidStore(format!(
                "spine checkpoint ParseStack mismatch for {}",
                checkpoint.checkpoint_id
            )));
        }

        let parse_stack = replay_from_events(
            &archive,
            &events,
            &mems,
            &raw_live,
            Some(&checkpoint.parse_stack),
            Some(checkpoint.token_seq),
        )?;
        Ok(Self {
            store,
            parse_stack,
            raw_len: u64::try_from(raw_live.len())
                .map_err(|_| SpineError::InvalidEvent("raw item count overflow".to_string()))?,
            raw_live,
            open_requests: BTreeMap::new(),
            control_call_ids: BTreeSet::new(),
            pending: None,
        })
    }

    pub(crate) fn render_tree(&self) -> Result<String, SpineError> {
        self.parse_stack.render_tree()
    }

    pub(crate) fn render_tree_with_current_annotation(
        &self,
        current_annotation: Option<&str>,
    ) -> Result<String, SpineError> {
        self.parse_stack
            .render_tree_with_current_annotation(current_annotation)
    }

    pub(crate) fn build_tree_snapshot(&self) -> Result<SpineTreeUpdateEvent, SpineError> {
        let nodes = self.parse_stack.tree_snapshot_nodes()?;
        let active_node_id = self.parse_stack.current_cursor_id()?.as_path();
        Ok(SpineTreeUpdateEvent {
            snapshot_seq: self.store.next_event_seq()?,
            active_node_id,
            nodes,
        })
    }

    pub(crate) fn current_open_index(&self) -> Result<usize, SpineError> {
        Ok(self.parse_stack.current_open_meta()?.index)
    }

    pub(crate) fn current_open_input_tokens(&self) -> Option<i64> {
        self.parse_stack
            .current_open_meta_opt()
            .and_then(|meta| meta.open_input_tokens)
    }

    fn current_close_open_meta(&self) -> Result<&TreeMeta, SpineError> {
        let Some(open_meta) = self.parse_stack.current_open_meta_opt() else {
            let cursor = self.parse_stack.current_cursor_id()?;
            if cursor.is_root_epoch() {
                return Err(SpineError::InvalidEvent(format!(
                    "cannot close root epoch cursor {cursor}"
                )));
            }
            return Err(SpineError::InvalidEvent(
                "spine.close requires a live open task".to_string(),
            ));
        };
        if open_meta.id.is_root_epoch() {
            return Err(SpineError::InvalidEvent(
                "cannot close root epoch".to_string(),
            ));
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
        self.raw_live.extend(std::iter::repeat(true).take(count));
        Ok(())
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
            && matches!(
                name.as_str(),
                SPINE_TOOL_TREE | SPINE_TOOL_OPEN | SPINE_TOOL_CLOSE
            )
        {
            self.control_call_ids.insert(call_id.clone());
            if name == SPINE_TOOL_OPEN {
                if self.open_requests.contains_key(call_id) {
                    return Err(SpineError::InvalidEvent(format!(
                        "duplicate spine.open request anchor for {call_id}"
                    )));
                }
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
        if let ResponseItem::FunctionCallOutput { call_id, .. } = item
            && (self.control_call_ids.contains(call_id)
                || self
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.call_id == *call_id))
        {
            return Ok(());
        }
        self.append_and_shift_msg(&msg)
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
            self.store.next_event_seq()?,
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
            self.store.next_event_seq()?,
            &self.raw_live,
            &self.parse_stack,
            context,
        )?;
        checkpoint.checkpoint_id = "initial".to_string();
        self.store.write_initial_checkpoint(&checkpoint)
    }

    fn append_msg_event(&self, msg: &PendingMsg) -> Result<u64, SpineError> {
        self.store.append_event(&KEvent::Msg {
            raw_ordinal: msg.raw_ordinal,
            context_index: msg.context_index,
            from_user: msg.from_user,
        })
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

    fn append_and_shift_msg(&mut self, msg: &PendingMsg) -> Result<(), SpineError> {
        self.append_msg_event(msg)?;
        self.push_msg_token(msg)
    }

    pub(crate) fn stage_open(
        &mut self,
        call_id: String,
        summary: String,
    ) -> Result<(), SpineError> {
        let summary = summary.trim().to_string();
        if summary.is_empty() {
            return Err(SpineError::InvalidEvent(
                "spine.open summary must not be empty".to_string(),
            ));
        }
        let anchor = self.open_requests.remove(&call_id).ok_or_else(|| {
            SpineError::InvalidEvent(format!("missing spine.open request anchor for {call_id}"))
        })?;
        self.stage(PendingTransition {
            call_id,
            op: SpineOp::Open,
            summary: Some(summary),
            boundary: Some(anchor.raw_ordinal),
            index: Some(anchor.context_index),
            instruction: None,
        })
    }

    pub(crate) fn stage_close(
        &mut self,
        call_id: String,
        instruction: Option<String>,
    ) -> Result<(), SpineError> {
        self.stage(PendingTransition {
            call_id,
            op: SpineOp::Close,
            summary: None,
            boundary: None,
            index: None,
            instruction,
        })
    }

    fn stage(&mut self, pending: PendingTransition) -> Result<(), SpineError> {
        if self.pending.is_some() {
            return Err(SpineError::InvalidEvent(
                "another spine transition is already pending".to_string(),
            ));
        }
        self.pending = Some(pending);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn maybe_commit_output(
        &mut self,
        call_id: &str,
        close_compact: Option<SpineCloseCompact>,
    ) -> Result<Option<SpineCommitKind>, SpineError> {
        self.maybe_commit_output_with_open_input_tokens(call_id, close_compact, None)
    }

    pub(crate) fn maybe_commit_output_with_open_input_tokens(
        &mut self,
        call_id: &str,
        close_compact: Option<SpineCloseCompact>,
        input_tokens: Option<i64>,
    ) -> Result<Option<SpineCommitKind>, SpineError> {
        let Some(pending) = self.pending.clone() else {
            return Ok(None);
        };
        if pending.call_id != call_id {
            return Ok(None);
        }
        match pending.op {
            SpineOp::Open => {
                let child = self.parse_stack.next_child_id()?;
                let boundary = pending.boundary.ok_or_else(|| {
                    SpineError::InvalidEvent("missing spine.open boundary".to_string())
                })?;
                let index = pending.index.ok_or_else(|| {
                    SpineError::InvalidEvent("missing spine.open context index".to_string())
                })?;
                let summary = pending.summary.ok_or_else(|| {
                    SpineError::InvalidEvent("missing spine.open summary".to_string())
                })?;
                let event = KEvent::Open {
                    child: child.clone(),
                    boundary,
                    index,
                    summary: summary.clone(),
                    open_input_tokens: input_tokens,
                };
                self.parse_stack.shift(
                    SpineToken::Open {
                        meta: tree_meta_with_open_input_tokens(
                            &self.archive(),
                            child.clone(),
                            index,
                            summary.clone(),
                            input_tokens,
                        )?,
                    },
                    &self.archive(),
                )?;
                self.store.append_event(&event)?;
            }
            SpineOp::Close => {
                let open_meta = self.current_close_open_meta()?.clone();
                let node = open_meta.id.clone();
                if !self.parse_stack.current_open_has_nodes()? {
                    return Err(SpineError::InvalidEvent(
                        "spine.close requires non-empty live suffix".to_string(),
                    ));
                }
                let suffix_start = open_meta.index;
                let summary = open_meta.summary.clone();
                let event = KEvent::Close {
                    node: node.clone(),
                    boundary: self.raw_len,
                    summary: summary.clone(),
                    instruction: pending.instruction.clone(),
                    close_input_tokens: input_tokens,
                };
                let close_compact = close_compact.ok_or_else(|| {
                    SpineError::InvalidEvent(
                        "spine.close requires a completed suffix compact".to_string(),
                    )
                })?;
                let seq = self.store.next_event_seq()?;
                if close_compact.source_context_range.start != suffix_start {
                    return Err(SpineError::InvalidEvent(format!(
                        "spine.close compact context range starts at {}, expected suffix start {suffix_start}",
                        close_compact.source_context_range.start
                    )));
                }
                let mem = self.stage_close_mem(&open_meta, close_compact, input_tokens)?;
                let body = self.store.read_memory_body(&mem)?;
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
                    mem.memory_output_tokens,
                );
                let mut staged_parse_stack = self.parse_stack.clone();
                staged_parse_stack.shift(SpineToken::Close { memory }, &self.archive())?;
                self.store.append_mem(&mem)?;
                self.store.append_event(&event)?;
                self.parse_stack = staged_parse_stack;
                self.pending = None;
                return Ok(Some(SpineCommitKind::Close {
                    suffix_start,
                    replacement: vec![memory_response_item(&body)],
                }));
            }
        }
        self.pending = None;
        Ok(Some(match pending.op {
            SpineOp::Open => {
                let open_request_index = pending.index.ok_or_else(|| {
                    SpineError::InvalidEvent("missing spine.open context index".to_string())
                })?;
                SpineCommitKind::Open {
                    open_request_index: usize::try_from(open_request_index).map_err(|_| {
                        SpineError::InvalidEvent("spine.open context index overflow".to_string())
                    })?,
                }
            }
            SpineOp::Close => unreachable!("close returns early with suffix replacement"),
        }))
    }

    pub(crate) fn pending_commit(
        &self,
        call_id: &str,
    ) -> Result<Option<SpinePendingCommit>, SpineError> {
        let Some(pending) = self.pending.as_ref() else {
            return Ok(None);
        };
        if pending.call_id != call_id {
            return Ok(None);
        }
        Ok(Some(match pending.op {
            SpineOp::Open => SpinePendingCommit::Open,
            SpineOp::Close => {
                let open_meta = self.current_close_open_meta()?;
                SpinePendingCommit::Close {
                    node: open_meta.id.clone(),
                    suffix_start: open_meta.index,
                    instruction: pending.instruction.clone(),
                }
            }
        }))
    }

    #[cfg(test)]
    pub(crate) fn root_compact(
        &mut self,
        body: String,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<Vec<ResponseItem>, SpineError> {
        self.root_compact_with_next_open_input_tokens(body, raw_items, None)
    }

    pub(crate) fn root_compact_with_next_open_input_tokens(
        &mut self,
        body: String,
        raw_items: &[Option<ResponseItem>],
        next_open_input_tokens: Option<i64>,
    ) -> Result<Vec<ResponseItem>, SpineError> {
        if body.trim().is_empty() {
            return Err(SpineError::InvalidEvent(
                "spine root compact memory body must not be empty".to_string(),
            ));
        }
        let source_context_end = self.materialize_history(raw_items)?.len();
        let node = self.parse_stack.current_root_epoch_id()?;
        let compact_id = format!("root-{}-{}", node.as_path().replace('.', "-"), self.raw_len);
        let body_path = self.store.write_memory_body(&compact_id, &body)?;
        let raw_live_hash = hash_raw_live(&self.raw_live);
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
            close_input_tokens: next_open_input_tokens,
            memory_output_tokens: None,
            body_path,
            body_hash: sha1_hex(body.as_bytes()),
        };
        let seq = self.store.next_event_seq()?;
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
            mem.memory_output_tokens,
        );

        // Probe first because source_context_range records the pre-compact source
        // span, while next_open_index is the post-compact h(PS) materialized len.
        let mut probe_parse_stack = self.parse_stack.clone();
        probe_parse_stack.shift(
            SpineToken::Compact {
                memory: memory.clone(),
                next_open_index: 0,
                next_open_input_tokens,
            },
            &self.archive(),
        )?;
        let next_open_index = render_parse_stack_to_context(&probe_parse_stack, raw_items)?.len();

        let mut staged_parse_stack = self.parse_stack.clone();
        staged_parse_stack.shift(
            SpineToken::Compact {
                memory,
                next_open_index,
                next_open_input_tokens,
            },
            &self.archive(),
        )?;
        let materialized = render_parse_stack_to_context(&staged_parse_stack, raw_items)?;
        let current_open_index = staged_parse_stack.current_open_meta()?.index;
        if current_open_index != materialized.len() {
            return Err(SpineError::InvalidEvent(format!(
                "spine root compact open index {current_open_index} does not match materialized history length {}",
                materialized.len()
            )));
        }
        self.store.append_mem(&mem)?;
        self.store.append_event(&KEvent::RootCompact {
            node: node.clone(),
            boundary: self.raw_len,
            mem: compact_id.clone(),
            next_open_index: u64::try_from(next_open_index)
                .map_err(|_| SpineError::InvalidEvent("root open index overflow".to_string()))?,
            raw_live_hash,
            next_open_input_tokens,
        })?;
        self.parse_stack = staged_parse_stack;
        self.pending = None;
        Ok(materialized)
    }

    fn stage_close_mem(
        &self,
        open_meta: &TreeMeta,
        close_compact: SpineCloseCompact,
        close_input_tokens: Option<i64>,
    ) -> Result<MemRecord, SpineError> {
        let node_id = open_meta.id.clone();
        let raw_start = self.open_raw_start(&node_id)?;
        let end = self.raw_len;
        let compact_id = format!(
            "mem-{}-{}-{}",
            node_id.as_path().replace('.', "-"),
            raw_start,
            end
        );
        let body_path = self
            .store
            .write_memory_body(&compact_id, &close_compact.body)?;
        let mem = MemRecord {
            compact_id: compact_id.clone(),
            kind: MemKind::Suffix,
            node: node_id.clone(),
            raw_start,
            raw_end: end,
            context_start: close_compact.source_context_range.start,
            context_end: close_compact.source_context_range.end,
            raw_live_hash: None,
            open_input_tokens: open_meta.open_input_tokens,
            close_input_tokens,
            memory_output_tokens: close_compact.memory_output_tokens,
            body_path,
            body_hash: sha1_hex(close_compact.body.as_bytes()),
        };
        Ok(mem)
    }

    fn open_raw_start(&self, node_id: &NodeId) -> Result<u64, SpineError> {
        let events = self.store.events()?;
        if let Some(boundary) = events.iter().rev().find_map(|event| match &event.event {
            KEvent::Open {
                child, boundary, ..
            } if child == node_id => Some(*boundary),
            _ => None,
        }) {
            return Ok(boundary);
        }
        let Some(parent) = node_id.parent() else {
            return Err(SpineError::InvalidEvent(format!(
                "missing open event for {node_id}"
            )));
        };
        if parent.is_root_epoch() && node_id.0.last() == Some(&1) {
            let root_epoch =
                parent.0.first().copied().ok_or_else(|| {
                    SpineError::InvalidEvent("root epoch id is empty".to_string())
                })?;
            let Some(previous_root_epoch) = root_epoch.checked_sub(1) else {
                return Err(SpineError::InvalidEvent(format!(
                    "missing open event for {node_id}"
                )));
            };
            let compacted_parent = NodeId::root_epoch(previous_root_epoch);
            return events
                .into_iter()
                .rev()
                .find_map(|event| match event.event {
                    KEvent::RootCompact { node, boundary, .. }
                        if node == compacted_parent && parent.child(1) == *node_id =>
                    {
                        Some(boundary)
                    }
                    _ => None,
                })
                .ok_or_else(|| {
                    SpineError::InvalidEvent(format!("missing open event for {node_id}"))
                });
        }
        Err(SpineError::InvalidEvent(format!(
            "missing open event for {node_id}"
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
        let (spine_control_call_ids, function_call_ids) = raw_items
            .iter()
            .filter_map(|item| match item.as_ref()? {
                ResponseItem::FunctionCall {
                    call_id,
                    namespace: Some(namespace),
                    name,
                    ..
                } if namespace == SPINE_NAMESPACE
                    && matches!(
                        name.as_str(),
                        SPINE_TOOL_TREE | SPINE_TOOL_OPEN | SPINE_TOOL_CLOSE
                    ) =>
                {
                    Some((call_id.clone(), true))
                }
                ResponseItem::FunctionCall { call_id, .. } => Some((call_id.clone(), false)),
                _ => None,
            })
            .fold(
                (BTreeSet::new(), BTreeSet::new()),
                |(mut spine_call_ids, mut all_call_ids), (call_id, is_spine)| {
                    if is_spine {
                        spine_call_ids.insert(call_id.clone());
                    }
                    all_call_ids.insert(call_id);
                    (spine_call_ids, all_call_ids)
                },
            );
        let mut covered = vec![false; raw_items.len()];
        for event in self.store.events()? {
            if !event.allowed_by(RawMask::new(&self.raw_live))? {
                continue;
            }
            match event.event {
                KEvent::Msg { raw_ordinal, .. } => {
                    mark_raw_covered(&mut covered, raw_ordinal)?;
                }
                KEvent::Open {
                    child,
                    boundary,
                    summary,
                    ..
                } => {
                    if !(summary == "root"
                        && child.parent().is_some_and(|parent| parent.is_root_epoch()))
                    {
                        mark_raw_covered(&mut covered, boundary)?;
                    }
                }
                KEvent::Close { boundary, .. } | KEvent::RootCompact { boundary, .. } => {
                    mark_raw_prefix_covered(&mut covered, boundary)?;
                }
                KEvent::Init { .. } => {}
            }
        }
        for (index, item) in raw_items.iter().enumerate() {
            if item.as_ref().is_some_and(|item| {
                raw_item_requires_spine_coverage(item, &spine_control_call_ids, &function_call_ids)
            }) && !covered[index]
            {
                return Err(SpineError::InvalidStore(format!(
                    "spine sidecar is missing token coverage for raw ordinal {index}"
                )));
            }
        }
        Ok(())
    }
}

fn replay_from_events(
    archive: &SpineArchive,
    events: &[LoggedKEvent],
    mems: &[MemRecord],
    raw_live: &[bool],
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
        return parse_stack_from_events(&events, archive, mems, raw_mask);
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
        if !event.allowed_by(raw_mask)? {
            continue;
        }
        parse_stack.shift(event_to_token(event, archive, &mem_map, raw_mask)?, archive)?;
    }
    Ok(parse_stack)
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

fn raw_item_requires_spine_coverage(
    item: &ResponseItem,
    spine_control_call_ids: &BTreeSet<String>,
    function_call_ids: &BTreeSet<String>,
) -> bool {
    match item {
        ResponseItem::FunctionCall {
            call_id,
            namespace: Some(namespace),
            name,
            ..
        } if namespace == SPINE_NAMESPACE
            && matches!(
                name.as_str(),
                SPINE_TOOL_TREE | SPINE_TOOL_OPEN | SPINE_TOOL_CLOSE
            ) =>
        {
            !spine_control_call_ids.contains(call_id)
        }
        ResponseItem::FunctionCallOutput { call_id, .. } => {
            function_call_ids.contains(call_id) && !spine_control_call_ids.contains(call_id)
        }
        ResponseItem::Other | ResponseItem::CompactionTrigger => false,
        _ => true,
    }
}

pub(crate) fn is_user_message(item: &ResponseItem) -> bool {
    matches!(item, ResponseItem::Message { role, .. } if role == "user")
}

#[derive(Debug, Error)]
pub(crate) enum SpineError {
    #[error("spine store error: {0}")]
    InvalidStore(String),
    #[error("spine event error: {0}")]
    InvalidEvent(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
