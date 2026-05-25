use codex_protocol::models::ResponseItem;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::ops::Range;
use std::path::Path;
use thiserror::Error;

use crate::spine::archive::SpineArchive;
use crate::spine::archive::memory_ref;
use crate::spine::archive::tree_meta;
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
}

#[derive(Debug)]
pub(crate) struct SpineSessionState {
    raw_len: u64,
    runtime: Option<SpineRuntime>,
}

impl SpineSessionState {
    pub(crate) fn new() -> Self {
        Self {
            raw_len: 0,
            runtime: None,
        }
    }

    pub(crate) fn runtime(&self) -> Option<&SpineRuntime> {
        self.runtime.as_ref()
    }

    pub(crate) fn runtime_mut(&mut self) -> Option<&mut SpineRuntime> {
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
        Ok(())
    }

    pub(crate) fn observe_raw_items(&mut self, count: usize) -> Result<(), SpineError> {
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
        if self.runtime.is_none() {
            self.runtime = Some(SpineRuntime::load_or_create(rollout_path, self.raw_len)?);
        }
        Ok(())
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
        Self::load_with_raw_live_for_rollout(
            SpineStore::for_rollout(rollout_path)?,
            raw_items.iter().map(Option::is_some).collect(),
            rollback_cuts,
            rollout_path,
            raw_items,
        )
        .map(Some)
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

    fn tree_meta_for_child(
        &self,
        child: NodeId,
        index: u64,
        summary: String,
    ) -> Result<TreeMeta, SpineError> {
        tree_meta(&self.archive(), child, index, summary)
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

    pub(crate) fn maybe_commit_output(
        &mut self,
        call_id: &str,
        close_compact: Option<SpineCloseCompact>,
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
                };
                self.parse_stack.shift(
                    SpineToken::Open {
                        meta: self.tree_meta_for_child(child.clone(), index, summary.clone())?,
                    },
                    &self.archive(),
                )?;
                self.store.append_event(&event)?;
            }
            SpineOp::Close => {
                let open_meta = self.parse_stack.current_open_meta()?.clone();
                let node = open_meta.id.clone();
                if node.is_root_epoch() {
                    return Err(SpineError::InvalidEvent(
                        "cannot close root epoch".to_string(),
                    ));
                }
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
                let mem = self.stage_close_mem(&open_meta, close_compact)?;
                let body = self.store.read_memory_body(&mem)?;
                let memory = memory_ref(
                    &self.archive(),
                    mem.compact_id.clone(),
                    mem.node.clone(),
                    mem.body_hash.clone(),
                    mem.raw_start..mem.raw_end,
                    mem.context_start..mem.context_end,
                    seq..seq + 1,
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
                let open_meta = self.parse_stack.current_open_meta()?;
                SpinePendingCommit::Close {
                    node: open_meta.id.clone(),
                    suffix_start: open_meta.index,
                    instruction: pending.instruction.clone(),
                }
            }
        }))
    }

    pub(crate) fn root_compact(
        &mut self,
        body: String,
        next_open_index: usize,
    ) -> Result<(), SpineError> {
        if body.trim().is_empty() {
            return Err(SpineError::InvalidEvent(
                "spine root compact memory body must not be empty".to_string(),
            ));
        }
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
            context_end: next_open_index,
            raw_live_hash: Some(raw_live_hash.clone()),
            body_path,
            body_hash: sha1_hex(body.as_bytes()),
        };
        let seq = self.store.next_event_seq()?;
        let mut staged_parse_stack = self.parse_stack.clone();
        staged_parse_stack.shift(
            SpineToken::Compact {
                memory: memory_ref(
                    &self.archive(),
                    mem.compact_id.clone(),
                    mem.node.clone(),
                    mem.body_hash.clone(),
                    mem.raw_start..mem.raw_end,
                    mem.context_start..mem.context_end,
                    seq..seq + 1,
                ),
                next_open_index,
            },
            &self.archive(),
        )?;
        self.store.append_mem(&mem)?;
        self.store.append_event(&KEvent::RootCompact {
            node: node.clone(),
            boundary: self.raw_len,
            mem: compact_id.clone(),
            next_open_index: u64::try_from(next_open_index)
                .map_err(|_| SpineError::InvalidEvent("root open index overflow".to_string()))?,
            raw_live_hash,
        })?;
        self.parse_stack = staged_parse_stack;
        self.pending = None;
        Ok(())
    }

    fn stage_close_mem(
        &self,
        open_meta: &TreeMeta,
        close_compact: SpineCloseCompact,
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
            body_path,
            body_hash: sha1_hex(close_compact.body.as_bytes()),
        };
        Ok(mem)
    }

    fn open_raw_start(&self, node_id: &NodeId) -> Result<u64, SpineError> {
        self.store
            .events()?
            .into_iter()
            .rev()
            .find_map(|event| match event.event {
                KEvent::Open {
                    child, boundary, ..
                } if &child == node_id => Some(boundary),
                _ => None,
            })
            .ok_or_else(|| SpineError::InvalidEvent(format!("missing open event for {node_id}")))
    }

    pub(crate) fn materialize_history(
        &self,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<Vec<ResponseItem>, SpineError> {
        render_parse_stack_to_context(&self.parse_stack, raw_items)
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
