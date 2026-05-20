use codex_protocol::models::ResponseItem;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fmt;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use thiserror::Error;

pub(crate) const SPINE_NAMESPACE: &str = "spine";
pub(crate) const SPINE_TOOL_TREE: &str = "tree";
pub(crate) const SPINE_TOOL_OPEN: &str = "open";
pub(crate) const SPINE_TOOL_CLOSE: &str = "close";

const LOCATOR_VERSION: u32 = 1;
const TREE_FILE: &str = "tree.jsonl";
const MEM_FILE: &str = "mem.jsonl";
const BODY_DIR: &str = "memory";

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct NodeId(Vec<u32>);

impl NodeId {
    fn root_epoch(index: u32) -> Self {
        Self(vec![index])
    }

    fn child(&self, index: u32) -> Self {
        let mut path = self.0.clone();
        path.push(index);
        Self(path)
    }

    fn parent(&self) -> Option<Self> {
        (self.0.len() > 1).then(|| Self(self.0[..self.0.len() - 1].to_vec()))
    }

    fn is_root_epoch(&self) -> bool {
        self.0.len() == 1
    }

    fn as_path(&self) -> String {
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
enum NodeStatus {
    Live,
    Suspended,
    Closed,
}

#[derive(Clone, Debug)]
struct Node {
    id: NodeId,
    children: Vec<NodeId>,
    status: NodeStatus,
    raw_start: u64,
    raw_end: Option<u64>,
    summary: Option<String>,
    mem: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct SpineState {
    nodes: BTreeMap<NodeId, Node>,
    stack: Vec<NodeId>,
    root_mem: Option<String>,
}

impl SpineState {
    fn new(raw_start: u64) -> Self {
        let epoch = NodeId::root_epoch(1);
        let leaf = epoch.child(1);
        let mut nodes = BTreeMap::new();
        nodes.insert(
            epoch.clone(),
            Node {
                id: epoch.clone(),
                children: vec![leaf.clone()],
                status: NodeStatus::Suspended,
                raw_start,
                raw_end: None,
                summary: None,
                mem: None,
            },
        );
        nodes.insert(
            leaf.clone(),
            Node {
                id: leaf.clone(),
                children: Vec::new(),
                status: NodeStatus::Live,
                raw_start,
                raw_end: None,
                summary: None,
                mem: None,
            },
        );
        Self {
            nodes,
            stack: vec![epoch, leaf],
            root_mem: None,
        }
    }

    fn cursor(&self) -> &NodeId {
        self.stack.last().expect("spine stack is never empty")
    }

    fn next_child_id(&self, parent: &NodeId) -> Result<NodeId, SpineError> {
        let parent_node = self
            .nodes
            .get(parent)
            .ok_or_else(|| SpineError::InvalidEvent(format!("unknown parent {parent}")))?;
        let index = u32::try_from(parent_node.children.len() + 1)
            .map_err(|_| SpineError::InvalidEvent("too many child nodes".to_string()))?;
        Ok(parent.child(index))
    }

    fn open(&mut self, child: NodeId, boundary: u64) -> Result<(), SpineError> {
        let parent = self.cursor().clone();
        let expected = self.next_child_id(&parent)?;
        if child != expected {
            return Err(SpineError::InvalidEvent(format!(
                "open child {child} does not match expected {expected}"
            )));
        }
        let parent_node = self
            .nodes
            .get_mut(&parent)
            .ok_or_else(|| SpineError::InvalidEvent(format!("open parent {parent} is missing")))?;
        if parent_node.status != NodeStatus::Live {
            return Err(SpineError::InvalidEvent(format!(
                "open parent {parent} is not live"
            )));
        }
        parent_node.status = NodeStatus::Suspended;
        parent_node.children.push(child.clone());
        self.nodes.insert(
            child.clone(),
            Node {
                id: child.clone(),
                children: Vec::new(),
                status: NodeStatus::Live,
                raw_start: boundary,
                raw_end: None,
                summary: None,
                mem: None,
            },
        );
        self.stack.push(child);
        Ok(())
    }

    fn close(&mut self, node: &NodeId, boundary: u64, summary: String) -> Result<(), SpineError> {
        if self.cursor() != node {
            return Err(SpineError::InvalidEvent(format!(
                "close node {node} is not current cursor {}",
                self.cursor()
            )));
        }
        if node.is_root_epoch() {
            return Err(SpineError::InvalidEvent(
                "cannot close root epoch".to_string(),
            ));
        }
        let parent = node
            .parent()
            .ok_or_else(|| SpineError::InvalidEvent("closed node has no parent".to_string()))?;
        let closing = self
            .nodes
            .get_mut(node)
            .ok_or_else(|| SpineError::InvalidEvent(format!("close node {node} is missing")))?;
        if closing.status != NodeStatus::Live {
            return Err(SpineError::InvalidEvent(format!(
                "close node {node} is not live"
            )));
        }
        closing.status = NodeStatus::Closed;
        closing.raw_end = Some(boundary);
        closing.summary = Some(summary);
        self.stack.pop();
        let parent_node = self
            .nodes
            .get_mut(&parent)
            .ok_or_else(|| SpineError::InvalidEvent(format!("close parent {parent} is missing")))?;
        parent_node.status = NodeStatus::Live;
        Ok(())
    }

    fn root_compact(
        &mut self,
        node: &NodeId,
        boundary: u64,
        mem: String,
    ) -> Result<(), SpineError> {
        let current_epoch = self
            .stack
            .first()
            .cloned()
            .ok_or_else(|| SpineError::InvalidEvent("missing root epoch".to_string()))?;
        if node != &current_epoch {
            return Err(SpineError::InvalidEvent(format!(
                "root compact node {node} is not current root epoch {current_epoch}"
            )));
        }
        if let Some(epoch) = self.nodes.get_mut(node) {
            epoch.status = NodeStatus::Closed;
            epoch.raw_end = Some(boundary);
            epoch.mem = Some(mem.clone());
        }
        for item in self.stack.iter().skip(1) {
            if let Some(node) = self.nodes.get_mut(item) {
                node.status = NodeStatus::Closed;
                node.raw_end.get_or_insert(boundary);
            }
        }
        let next_index = self
            .nodes
            .keys()
            .filter(|id| id.is_root_epoch())
            .map(|id| id.0[0])
            .max()
            .unwrap_or(0)
            + 1;
        let epoch = NodeId::root_epoch(next_index);
        let leaf = epoch.child(1);
        self.nodes.insert(
            epoch.clone(),
            Node {
                id: epoch.clone(),
                children: vec![leaf.clone()],
                status: NodeStatus::Suspended,
                raw_start: boundary,
                raw_end: None,
                summary: None,
                mem: None,
            },
        );
        self.nodes.insert(
            leaf.clone(),
            Node {
                id: leaf.clone(),
                children: Vec::new(),
                status: NodeStatus::Live,
                raw_start: boundary,
                raw_end: None,
                summary: None,
                mem: None,
            },
        );
        self.stack = vec![epoch, leaf];
        self.root_mem = Some(mem);
        Ok(())
    }

    fn visible_nodes(&self) -> Vec<NodeId> {
        let mut visible = BTreeSet::new();
        for node_id in &self.stack {
            visible.insert(node_id.clone());
            if let Some(parent) = node_id.parent()
                && let Some(parent_node) = self.nodes.get(&parent)
            {
                for sibling in &parent_node.children {
                    if sibling < node_id {
                        visible.insert(sibling.clone());
                    }
                }
            }
        }
        if let Some(cursor) = self.nodes.get(self.cursor()) {
            for child in &cursor.children {
                if self
                    .nodes
                    .get(child)
                    .is_some_and(|node| node.status == NodeStatus::Closed)
                {
                    visible.insert(child.clone());
                }
            }
        }
        visible.into_iter().collect()
    }

    pub(crate) fn render_tree(&self) -> String {
        let visible = self.visible_nodes().into_iter().collect::<BTreeSet<_>>();
        let mut lines = Vec::new();
        for node in self.nodes.values() {
            if !visible.contains(&node.id) {
                continue;
            }
            let depth = node.id.0.len().saturating_sub(1);
            let marker = match node.status {
                NodeStatus::Live => "Current",
                NodeStatus::Suspended => "Open",
                NodeStatus::Closed => "Done",
            };
            let label = node.summary.as_deref().unwrap_or("");
            lines.push(format!(
                "{}[{}] {} {}",
                "  ".repeat(depth),
                node.id,
                marker,
                label
            ));
        }
        lines.join("\n")
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum KEvent {
    Init {
        raw_start: u64,
    },
    Open {
        child: NodeId,
        boundary: u64,
    },
    Close {
        node: NodeId,
        boundary: u64,
        summary: String,
        instruction: Option<String>,
    },
    RootCompact {
        node: NodeId,
        boundary: u64,
        mem: String,
        raw_live_hash: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct MemRecord {
    compact_id: String,
    kind: MemKind,
    node: NodeId,
    start: u64,
    end: u64,
    #[serde(default)]
    raw_live_hash: Option<String>,
    body_path: String,
    body_hash: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MemKind {
    Suffix,
    RootEpoch,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Locator {
    version: u32,
    base: String,
}

#[derive(Clone, Debug)]
pub(crate) struct SpineStore {
    root: PathBuf,
}

#[derive(Clone, Copy)]
struct RawMask<'a> {
    live: Option<&'a [bool]>,
}

impl<'a> RawMask<'a> {
    fn new(live: &'a [bool]) -> Self {
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

impl SpineStore {
    pub(crate) fn for_rollout(rollout_path: &Path) -> Result<Self, SpineError> {
        let locator_path = locator_path(rollout_path)?;
        let locator: Locator = read_json_file(&locator_path)?;
        if locator.version != LOCATOR_VERSION {
            return Err(SpineError::InvalidStore(format!(
                "unsupported spine locator version {}",
                locator.version
            )));
        }
        Ok(Self {
            root: rollout_parent(rollout_path)?.join(locator.base),
        })
    }

    pub(crate) fn create_for_rollout(rollout_path: &Path) -> Result<Self, SpineError> {
        let parent = rollout_parent(rollout_path)?;
        let stem = rollout_stem(rollout_path)?;
        let root = parent.join(format!("spine-{stem}"));
        std::fs::create_dir_all(&root)?;
        let locator = Locator {
            version: LOCATOR_VERSION,
            base: root
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| SpineError::InvalidStore("invalid sidecar path".to_string()))?
                .to_string(),
        };
        write_json_file(&locator_path(rollout_path)?, &locator)?;
        Ok(Self { root })
    }

    pub(crate) fn clone_for_rollout_with_raw_live(
        source_rollout_path: &Path,
        target_rollout_path: &Path,
        raw_live: &[bool],
    ) -> Result<(), SpineError> {
        if !Self::has_for_rollout(source_rollout_path)? {
            return Ok(());
        }
        if Self::has_for_rollout(target_rollout_path)? {
            return Ok(());
        }
        let source = Self::for_rollout(source_rollout_path)?;
        let target = Self::create_for_rollout(target_rollout_path)?;
        let mask = RawMask::new(raw_live);
        for event in source.events()? {
            if event.allowed_by(mask)? {
                target.append_event(&event)?;
            }
        }
        for mem in source.mems()? {
            if mem.allowed_by(mask)? {
                let body = source.read_memory_body(&mem)?;
                let body_path = target.write_memory_body(&mem.compact_id, &body)?;
                let cloned = MemRecord { body_path, ..mem };
                target.append_mem(&cloned)?;
            }
        }
        Ok(())
    }

    pub(crate) fn has_for_rollout(rollout_path: &Path) -> Result<bool, SpineError> {
        Ok(locator_path(rollout_path)?.exists())
    }

    fn tree_path(&self) -> PathBuf {
        self.root.join(TREE_FILE)
    }

    fn mem_path(&self) -> PathBuf {
        self.root.join(MEM_FILE)
    }

    fn append_event(&self, event: &KEvent) -> Result<(), SpineError> {
        append_json_line(&self.tree_path(), event)
    }

    fn append_mem(&self, mem: &MemRecord) -> Result<(), SpineError> {
        append_json_line(&self.mem_path(), mem)
    }

    fn events(&self) -> Result<Vec<KEvent>, SpineError> {
        read_json_lines(&self.tree_path())
    }

    fn mems(&self) -> Result<Vec<MemRecord>, SpineError> {
        if !self.mem_path().exists() {
            return Ok(Vec::new());
        }
        read_json_lines(&self.mem_path())
    }

    fn write_memory_body(&self, compact_id: &str, body: &str) -> Result<String, SpineError> {
        let dir = self.root.join(BODY_DIR);
        std::fs::create_dir_all(&dir)?;
        let rel = format!("{BODY_DIR}/{compact_id}.md");
        std::fs::write(self.root.join(&rel), body)?;
        Ok(rel)
    }

    fn read_memory_body(&self, mem: &MemRecord) -> Result<String, SpineError> {
        let body = std::fs::read_to_string(self.root.join(&mem.body_path))?;
        if sha1_hex(body.as_bytes()) != mem.body_hash {
            return Err(SpineError::InvalidStore(format!(
                "memory body hash mismatch for {}",
                mem.compact_id
            )));
        }
        Ok(body)
    }
}

impl KEvent {
    fn allowed_by(&self, raw_mask: RawMask<'_>) -> Result<bool, SpineError> {
        match self {
            KEvent::Init { .. } => Ok(true),
            KEvent::Open { boundary, .. } | KEvent::Close { boundary, .. } => {
                raw_mask.boundary_live(*boundary)
            }
            KEvent::RootCompact {
                boundary,
                raw_live_hash,
                ..
            } => raw_mask.prefix_hash_matches(*boundary, raw_live_hash),
        }
    }
}

impl MemRecord {
    fn allowed_by(&self, raw_mask: RawMask<'_>) -> Result<bool, SpineError> {
        match self.kind {
            MemKind::Suffix => raw_mask.span_live(self.start, self.end),
            MemKind::RootEpoch => self
                .raw_live_hash
                .as_deref()
                .map(|hash| raw_mask.prefix_hash_matches(self.end, hash))
                .unwrap_or(Ok(false)),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SpineRuntime {
    store: SpineStore,
    state: SpineState,
    raw_len: u64,
    raw_live: Vec<bool>,
    pending: Option<PendingTransition>,
}

#[derive(Clone, Debug)]
struct PendingTransition {
    call_id: String,
    op: SpineOp,
    summary: Option<String>,
    instruction: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SpineOp {
    Open,
    Close,
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
            store.append_event(&KEvent::Init { raw_start: raw_len })?;
        }
        Self::load(store, raw_len)
    }

    pub(crate) fn load_for_rollout_items(
        rollout_path: &Path,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<Option<Self>, SpineError> {
        if !SpineStore::has_for_rollout(rollout_path)? {
            return Ok(None);
        }
        Self::load_with_raw_live(
            SpineStore::for_rollout(rollout_path)?,
            raw_items.iter().map(Option::is_some).collect(),
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
        let state = replay(&store.events()?, RawMask::new(&raw_live))?;
        Ok(Self {
            store,
            state,
            raw_len: u64::try_from(raw_live.len())
                .map_err(|_| SpineError::InvalidEvent("raw item count overflow".to_string()))?,
            raw_live,
            pending: None,
        })
    }

    pub(crate) fn state(&self) -> &SpineState {
        &self.state
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

    pub(crate) fn stage_open(&mut self, call_id: String) -> Result<(), SpineError> {
        self.stage(PendingTransition {
            call_id,
            op: SpineOp::Open,
            summary: None,
            instruction: None,
        })
    }

    pub(crate) fn stage_close(
        &mut self,
        call_id: String,
        summary: String,
        instruction: Option<String>,
    ) -> Result<(), SpineError> {
        if summary.trim().is_empty() {
            return Err(SpineError::InvalidEvent(
                "spine.close summary must not be empty".to_string(),
            ));
        }
        self.stage(PendingTransition {
            call_id,
            op: SpineOp::Close,
            summary: Some(summary),
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

    pub(crate) fn maybe_commit_output(&mut self, call_id: &str) -> Result<bool, SpineError> {
        let Some(pending) = self.pending.clone() else {
            return Ok(false);
        };
        if pending.call_id != call_id {
            return Ok(false);
        }
        match pending.op {
            SpineOp::Open => {
                let child = self.state.next_child_id(self.state.cursor())?;
                let event = KEvent::Open {
                    child: child.clone(),
                    boundary: self.raw_len,
                };
                self.state.open(child, self.raw_len)?;
                self.store.append_event(&event)?;
            }
            SpineOp::Close => {
                let node = self.state.cursor().clone();
                let summary = pending.summary.unwrap_or_default();
                let event = KEvent::Close {
                    node: node.clone(),
                    boundary: self.raw_len,
                    summary: summary.clone(),
                    instruction: pending.instruction.clone(),
                };
                self.state.close(&node, self.raw_len, summary)?;
                self.store.append_event(&event)?;
                self.install_synthetic_mem(node, pending.instruction)?;
            }
        }
        self.pending = None;
        Ok(true)
    }

    pub(crate) fn root_compact(&mut self, body: String) -> Result<(), SpineError> {
        if body.trim().is_empty() {
            return Ok(());
        }
        let node = self
            .state
            .stack
            .first()
            .cloned()
            .ok_or_else(|| SpineError::InvalidEvent("missing root epoch".to_string()))?;
        let compact_id = format!("root-{}-{}", node.as_path().replace('.', "-"), self.raw_len);
        let body_path = self.store.write_memory_body(&compact_id, &body)?;
        let raw_live_hash = hash_raw_live(&self.raw_live);
        let mem = MemRecord {
            compact_id: compact_id.clone(),
            kind: MemKind::RootEpoch,
            node: node.clone(),
            start: 0,
            end: self.raw_len,
            raw_live_hash: Some(raw_live_hash.clone()),
            body_path,
            body_hash: sha1_hex(body.as_bytes()),
        };
        self.store.append_mem(&mem)?;
        self.state
            .root_compact(&node, self.raw_len, compact_id.clone())?;
        self.store.append_event(&KEvent::RootCompact {
            node,
            boundary: self.raw_len,
            mem: compact_id,
            raw_live_hash,
        })?;
        Ok(())
    }

    fn install_synthetic_mem(
        &mut self,
        node_id: NodeId,
        instruction: Option<String>,
    ) -> Result<(), SpineError> {
        let node = self
            .state
            .nodes
            .get(&node_id)
            .ok_or_else(|| SpineError::InvalidEvent(format!("missing node {node_id}")))?;
        let end = node
            .raw_end
            .ok_or_else(|| SpineError::InvalidEvent(format!("node {node_id} is not closed")))?;
        let compact_id = format!(
            "mem-{}-{}-{}",
            node_id.as_path().replace('.', "-"),
            node.raw_start,
            end
        );
        let body = render_memory(node, instruction.as_deref());
        let body_path = self.store.write_memory_body(&compact_id, &body)?;
        let mem = MemRecord {
            compact_id: compact_id.clone(),
            kind: MemKind::Suffix,
            node: node_id.clone(),
            start: node.raw_start,
            end,
            raw_live_hash: None,
            body_path,
            body_hash: sha1_hex(body.as_bytes()),
        };
        self.store.append_mem(&mem)?;
        if let Some(node) = self.state.nodes.get_mut(&node_id) {
            node.mem = Some(compact_id);
        }
        Ok(())
    }

    pub(crate) fn materialize_history(
        &self,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<Vec<ResponseItem>, SpineError> {
        let raw_mask = RawMask::new(&self.raw_live);
        let mems = self
            .store
            .mems()?
            .into_iter()
            .filter(|mem| mem.allowed_by(raw_mask).unwrap_or(false))
            .collect::<Vec<_>>();
        let pi = project(self.raw_len, &self.state, &mems)?;
        let mut out = Vec::new();
        for seg in pi {
            match seg {
                Segment::Raw(start, end) => {
                    let start = usize::try_from(start)
                        .map_err(|_| SpineError::InvalidEvent("raw start overflow".to_string()))?;
                    let end = usize::try_from(end)
                        .map_err(|_| SpineError::InvalidEvent("raw end overflow".to_string()))?;
                    if end > raw_items.len() || start > end {
                        return Err(SpineError::InvalidEvent(
                            "raw segment outside rollout".to_string(),
                        ));
                    }
                    out.extend(raw_items[start..end].iter().flatten().cloned());
                }
                Segment::Mem(mem) => {
                    out.push(memory_response_item(&self.store.read_memory_body(&mem)?));
                }
            }
        }
        Ok(out)
    }
}

fn replay(events: &[KEvent], raw_mask: RawMask<'_>) -> Result<SpineState, SpineError> {
    let mut state = None;
    for event in events {
        if !event.allowed_by(raw_mask)? {
            continue;
        }
        match event {
            KEvent::Init { raw_start } => {
                if state.is_some() {
                    return Err(SpineError::InvalidEvent("duplicate init".to_string()));
                }
                state = Some(SpineState::new(*raw_start));
            }
            KEvent::Open { child, boundary } => state
                .as_mut()
                .ok_or_else(|| SpineError::InvalidEvent("open before init".to_string()))?
                .open(child.clone(), *boundary)?,
            KEvent::Close {
                node,
                boundary,
                summary,
                ..
            } => state
                .as_mut()
                .ok_or_else(|| SpineError::InvalidEvent("close before init".to_string()))?
                .close(node, *boundary, summary.clone())?,
            KEvent::RootCompact {
                node,
                boundary,
                mem,
                ..
            } => state
                .as_mut()
                .ok_or_else(|| SpineError::InvalidEvent("root compact before init".to_string()))?
                .root_compact(node, *boundary, mem.clone())?,
        }
    }
    state.ok_or_else(|| SpineError::InvalidEvent("missing init".to_string()))
}

#[derive(Clone)]
enum Segment {
    Raw(u64, u64),
    Mem(MemRecord),
}

fn project(
    raw_len: u64,
    state: &SpineState,
    mems: &[MemRecord],
) -> Result<Vec<Segment>, SpineError> {
    let visible = state.visible_nodes().into_iter().collect::<BTreeSet<_>>();
    let root_mem = state
        .root_mem
        .as_ref()
        .and_then(|compact_id| mems.iter().find(|mem| &mem.compact_id == compact_id))
        .cloned();
    let mut admitted = mems
        .iter()
        .filter(|mem| visible.contains(&mem.node))
        .filter(|mem| {
            state
                .nodes
                .get(&mem.node)
                .is_some_and(|node| node.status == NodeStatus::Closed)
        })
        .cloned()
        .collect::<Vec<_>>();
    if let Some(root_mem) = root_mem {
        admitted.push(root_mem);
    }
    admitted.sort_by_key(|mem| (mem.start, std::cmp::Reverse(mem.end)));
    let mut selected: Vec<MemRecord> = Vec::new();
    for mem in admitted {
        if mem.end > raw_len || mem.start >= mem.end {
            return Err(SpineError::InvalidEvent(format!(
                "invalid mem span [{}, {})",
                mem.start, mem.end
            )));
        }
        if selected
            .iter()
            .any(|prev| prev.start <= mem.start && mem.end <= prev.end)
        {
            continue;
        }
        if selected
            .iter()
            .any(|prev| mem.start < prev.end && prev.start < mem.end)
        {
            return Err(SpineError::InvalidEvent(
                "overlapping visible memory spans".to_string(),
            ));
        }
        selected.push(mem);
    }
    selected.sort_by_key(|mem| mem.start);

    let live_starts = state
        .visible_nodes()
        .into_iter()
        .filter_map(|id| state.nodes.get(&id))
        .filter(|node| matches!(node.status, NodeStatus::Live | NodeStatus::Suspended))
        .map(|node| node.raw_start)
        .collect::<Vec<_>>();
    for mem in &selected {
        if live_starts
            .iter()
            .any(|start| mem.start < *start && *start < mem.end)
        {
            return Err(SpineError::InvalidEvent(
                "memory span contains a live boundary".to_string(),
            ));
        }
    }

    let mut out = Vec::new();
    let mut cursor = 0;
    for mem in selected {
        if cursor < mem.start {
            out.push(Segment::Raw(cursor, mem.start));
        }
        cursor = mem.end;
        out.push(Segment::Mem(mem));
    }
    if cursor < raw_len {
        out.push(Segment::Raw(cursor, raw_len));
    }
    Ok(out)
}

fn render_memory(node: &Node, instruction: Option<&str>) -> String {
    let mut body = format!(
        "# Spine Memory {}\n\nSummary: {}\nRaw span: [{}, {})\n",
        node.id,
        node.summary.as_deref().unwrap_or(""),
        node.raw_start,
        node.raw_end.unwrap_or(node.raw_start)
    );
    if let Some(instruction) = instruction {
        body.push_str("\nCompact guidance: ");
        body.push_str(instruction);
        body.push('\n');
    }
    body
}

fn memory_response_item(body: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![codex_protocol::models::ContentItem::InputText {
            text: format!("<spine_memory runtime_generated=\"true\">\n{body}\n</spine_memory>"),
        }],
        phase: None,
    }
}

fn locator_path(rollout_path: &Path) -> Result<PathBuf, SpineError> {
    Ok(rollout_parent(rollout_path)?.join(format!("{}.spine.json", rollout_stem(rollout_path)?)))
}

fn rollout_parent(path: &Path) -> Result<&Path, SpineError> {
    path.parent()
        .ok_or_else(|| SpineError::InvalidStore("rollout path has no parent".to_string()))
}

fn rollout_stem(path: &Path) -> Result<String, SpineError> {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(ToString::to_string)
        .ok_or_else(|| SpineError::InvalidStore("rollout path has no UTF-8 stem".to_string()))
}

fn append_json_line<T: Serialize>(path: &Path, value: &T) -> Result<(), SpineError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n")?;
    Ok(())
}

fn read_json_lines<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Vec<T>, SpineError> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut out = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        out.push(serde_json::from_str(&line)?);
    }
    Ok(out)
}

fn read_json_file<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, SpineError> {
    let text = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&text)?)
}

fn write_json_file<T: Serialize>(path: &Path, value: &T) -> Result<(), SpineError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(value)? + "\n")?;
    Ok(())
}

fn sha1_hex(bytes: &[u8]) -> String {
    use sha1::Digest;
    let mut hasher = sha1::Sha1::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn hash_raw_live(raw_live: &[bool]) -> String {
    use sha1::Digest;
    let mut hasher = sha1::Sha1::new();
    for live in raw_live {
        hasher.update(if *live { b"1" } else { b"0" });
    }
    format!("{:x}", hasher.finalize())
}

fn hash_raw_live_prefix_all_true(len: usize) -> String {
    use sha1::Digest;
    let mut hasher = sha1::Sha1::new();
    for _ in 0..len {
        hasher.update(b"1");
    }
    format!("{:x}", hasher.finalize())
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
mod tests {
    use super::*;
    use codex_protocol::models::ContentItem;

    fn rollout_path(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("rollout.jsonl")
    }

    fn text_item(text: &str) -> ResponseItem {
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: text.to_string(),
            }],
            phase: None,
        }
    }

    #[test]
    fn open_close_replay_materializes_closed_child_memory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rollout = rollout_path(&dir);
        let raw = vec![Some(text_item("before")), Some(text_item("inside"))];

        let mut runtime = SpineRuntime::load_or_create(&rollout, 1).expect("create spine");
        runtime.stage_open("open".to_string()).expect("stage open");
        runtime.maybe_commit_output("open").expect("commit open");
        runtime.observe_raw_items(1).expect("observe child item");
        runtime
            .stage_close("close".to_string(), "child done".to_string(), None)
            .expect("stage close");
        runtime.maybe_commit_output("close").expect("commit close");

        let replayed = SpineRuntime::load_for_rollout(&rollout, runtime.raw_len)
            .expect("load spine")
            .expect("sidecar exists");
        let tree = replayed.state().render_tree();
        assert!(tree.contains("[1.1] Current"));
        assert!(tree.contains("[1.1.1] Done child done"));

        let materialized = replayed
            .materialize_history(&raw)
            .expect("materialize history");
        assert_eq!(materialized.len(), 2);
        assert_eq!(materialized[0], text_item("before"));
        assert!(matches!(
            &materialized[1],
            ResponseItem::Message { content, .. }
                if matches!(
                    content.as_slice(),
                    [ContentItem::InputText { text }]
                        if text.contains("Spine Memory 1.1.1")
                )
        ));
    }

    #[test]
    fn materialization_skips_rolled_back_raw_items_without_shifting_ordinals() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rollout = rollout_path(&dir);
        let raw = vec![
            Some(text_item("kept")),
            None,
            Some(text_item("after rollback")),
        ];

        let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
        runtime.observe_raw_items(3).expect("observe raw");
        let materialized = runtime.materialize_history(&raw).expect("materialize");

        assert_eq!(
            materialized,
            vec![text_item("kept"), text_item("after rollback")]
        );
    }

    #[test]
    fn rollback_skips_stale_spine_transition_events() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rollout = rollout_path(&dir);
        let raw = vec![Some(text_item("before")), None];

        let mut runtime = SpineRuntime::load_or_create(&rollout, 1).expect("create spine");
        runtime.stage_open("open".to_string()).expect("stage open");
        runtime.observe_raw_items(1).expect("record open output");
        runtime.maybe_commit_output("open").expect("commit open");

        let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw)
            .expect("load spine")
            .expect("sidecar exists");
        assert_eq!(replayed.state().render_tree(), "[1] Open \n  [1.1] Current ");
        assert_eq!(
            replayed.materialize_history(&raw).expect("materialize"),
            vec![text_item("before")]
        );
    }

    #[test]
    fn rollback_hole_rejects_suffix_memory_span() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rollout = rollout_path(&dir);
        let raw = vec![
            Some(text_item("before")),
            Some(text_item("open output")),
            None,
            Some(text_item("close output")),
        ];

        let mut runtime = SpineRuntime::load_or_create(&rollout, 1).expect("create spine");
        runtime.stage_open("open".to_string()).expect("stage open");
        runtime.observe_raw_items(1).expect("record open output");
        runtime.maybe_commit_output("open").expect("commit open");
        runtime.observe_raw_items(1).expect("record rolled-back child raw");
        runtime
            .stage_close("close".to_string(), "child done".to_string(), None)
            .expect("stage close");
        runtime.observe_raw_items(1).expect("record close output");
        runtime.maybe_commit_output("close").expect("commit close");

        let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw)
            .expect("load spine")
            .expect("sidecar exists");
        let materialized = replayed.materialize_history(&raw).expect("materialize");
        assert_eq!(
            materialized,
            vec![
                text_item("before"),
                text_item("open output"),
                text_item("close output"),
            ]
        );
    }

    #[test]
    fn root_compact_survives_rollback_without_new_raw_items() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rollout = rollout_path(&dir);
        let raw_after_rollback = vec![Some(text_item("kept")), None];

        let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
        runtime.observe_raw_items(2).expect("record raw");
        runtime.raw_live = vec![true, false];
        runtime
            .root_compact("root summary after rollback".to_string())
            .expect("compact root");

        let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw_after_rollback)
            .expect("load spine")
            .expect("sidecar exists");
        let materialized = replayed
            .materialize_history(&raw_after_rollback)
            .expect("materialize");
        assert_eq!(materialized.len(), 1);
        assert!(matches!(
            &materialized[0],
            ResponseItem::Message { content, .. }
                if matches!(
                    content.as_slice(),
                    [ContentItem::InputText { text }]
                        if text.contains("root summary after rollback")
                )
        ));
    }
}
