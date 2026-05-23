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
use std::ops::Range;
use std::path::Path;
use std::path::PathBuf;
use thiserror::Error;

pub(crate) const SPINE_NAMESPACE: &str = "spine";
pub(crate) const SPINE_TOOL_TREE: &str = "tree";
pub(crate) const SPINE_TOOL_OPEN: &str = "open";
pub(crate) const SPINE_TOOL_CLOSE: &str = "close";

const LOCATOR_VERSION: u32 = 1;
const CHECKPOINT_VERSION: u32 = 1;
const TREE_FILE: &str = "tree.jsonl";
const MEM_FILE: &str = "mem.jsonl";
const BODY_DIR: &str = "memory";
const CHECKPOINT_DIR: &str = "checkpoints";

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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum KEvent {
    Init {
        raw_start: u64,
    },
    Msg {
        raw_ordinal: u64,
        context_index: u64,
        from_user: bool,
    },
    Open {
        child: NodeId,
        boundary: u64,
        index: u64,
        summary: String,
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
        next_open_index: u64,
        raw_live_hash: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct LoggedKEvent {
    seq: u64,
    #[serde(flatten)]
    event: KEvent,
}

impl std::ops::Deref for LoggedKEvent {
    type Target = KEvent;

    fn deref(&self) -> &Self::Target {
        &self.event
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct MemRecord {
    compact_id: String,
    kind: MemKind,
    node: NodeId,
    raw_start: u64,
    raw_end: u64,
    context_start: usize,
    context_end: usize,
    #[serde(default)]
    raw_live_hash: Option<String>,
    body_path: String,
    body_hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct TreeMeta {
    id: NodeId,
    index: usize,
    summary: String,
    node_dir: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum SegRef {
    ResponseItem {
        raw_ordinal: u64,
        context_index: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct MemoryRef {
    compact_id: String,
    node_id: NodeId,
    body_path: PathBuf,
    body_hash: String,
    source_raw_range: Range<u64>,
    source_context_range: Range<usize>,
    source_token_seq: Range<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum SpineToken {
    Init {
        meta: TreeMeta,
    },
    Open {
        meta: TreeMeta,
    },
    Close {
        memory: MemoryRef,
    },
    Compact {
        memory: MemoryRef,
        next_open_index: usize,
    },
    Msg {
        seg: SegRef,
        from_user: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum ControlSymbol {
    Init(TreeMeta),
    Open(TreeMeta),
    Close(MemoryRef),
    Compact(MemoryRef, usize),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Symbol {
    Control(ControlSymbol),
    SpineTreeNode(SpineTreeNode),
    SpineTreeNodes(Vec<SpineTreeNode>),
    RootEpoches(Vec<RootEpoch>),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum SpineTreeNode {
    MsgAsLeafNode {
        msg: SegRef,
        from_user: bool,
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
struct RootEpoch {
    memory: MemoryRef,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct ParseStack {
    symbols: Vec<Symbol>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SpineCheckpoint {
    version: u32,
    checkpoint_id: String,
    rollout_path: String,
    raw_ordinal: u64,
    token_seq: u64,
    raw_live_hash: String,
    context_len: usize,
    cursor: String,
    parse_stack: ParseStack,
    parse_stack_symbols: Vec<String>,
    tree_meta: Vec<CheckpointTreeMeta>,
    memory_refs: Vec<CheckpointMemoryRef>,
    trajs_refs: Vec<CheckpointTrajsRef>,
    h_ps_hash: String,
    context_hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CheckpointTreeMeta {
    id: String,
    index: usize,
    summary: String,
    node_dir: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CheckpointMemoryRef {
    compact_id: String,
    node_id: String,
    body_path: String,
    body_hash: String,
    source_raw_start: u64,
    source_raw_end: u64,
    source_context_start: usize,
    source_context_end: usize,
    source_token_seq_start: u64,
    source_token_seq_end: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CheckpointTrajsRef {
    node_id: String,
    trajs_path: String,
}

impl ParseStack {
    fn new() -> Self {
        Self {
            symbols: Vec::new(),
        }
    }

    fn shift(&mut self, token: SpineToken, archive: &SpineArchive) -> Result<(), SpineError> {
        self.reduce_fixpoint(archive)?;
        let symbol = match token {
            SpineToken::Init { meta } => Symbol::Control(ControlSymbol::Init(meta)),
            SpineToken::Open { meta } => Symbol::Control(ControlSymbol::Open(meta)),
            SpineToken::Close { memory } => Symbol::Control(ControlSymbol::Close(memory)),
            SpineToken::Compact {
                memory,
                next_open_index,
            } => Symbol::Control(ControlSymbol::Compact(memory, next_open_index)),
            SpineToken::Msg { seg, from_user } => {
                Symbol::SpineTreeNode(SpineTreeNode::MsgAsLeafNode {
                    msg: seg,
                    from_user,
                })
            }
        };
        self.symbols.push(symbol);
        self.reduce_fixpoint(archive)
    }

    fn reduce_fixpoint(&mut self, archive: &SpineArchive) -> Result<(), SpineError> {
        loop {
            if self.reduce_task_tree(archive)? {
                continue;
            }
            if self.reduce_root_epoch(archive)? {
                continue;
            }
            if self.reduce_nodes_append() {
                continue;
            }
            if self.reduce_node_to_nodes() {
                continue;
            }
            break;
        }
        Ok(())
    }

    fn reduce_task_tree(&mut self, archive: &SpineArchive) -> Result<bool, SpineError> {
        let has_nodes = match self.symbols.get(..) {
            Some(
                [
                    ..,
                    Symbol::Control(ControlSymbol::Open(_)),
                    Symbol::SpineTreeNodes(_),
                    Symbol::Control(ControlSymbol::Close(_)),
                ],
            ) => true,
            Some(
                [
                    ..,
                    Symbol::Control(ControlSymbol::Open(_)),
                    Symbol::Control(ControlSymbol::Close(_)),
                ],
            ) => false,
            _ => return Ok(false),
        };
        let Some(Symbol::Control(ControlSymbol::Close(memory))) = self.symbols.pop() else {
            unreachable!("close symbol matched by reduce pattern")
        };
        let children = if has_nodes {
            let Some(Symbol::SpineTreeNodes(children)) = self.symbols.pop() else {
                unreachable!("nodes symbol matched by reduce pattern")
            };
            children
        } else {
            Vec::new()
        };
        let Some(Symbol::Control(ControlSymbol::Open(meta))) = self.symbols.pop() else {
            unreachable!("open symbol matched by reduce pattern")
        };
        let (memory_path, trajs_path) = archive_task_tree(archive, &meta, &children, &memory)?;
        self.symbols
            .push(Symbol::SpineTreeNode(SpineTreeNode::SpineTree {
                memory,
                meta,
                children,
                memory_path,
                trajs_path,
            }));
        Ok(true)
    }

    fn reduce_nodes_append(&mut self) -> bool {
        let Some([.., Symbol::SpineTreeNodes(_), Symbol::SpineTreeNode(_)]) = self.symbols.get(..)
        else {
            return false;
        };
        let node = self
            .symbols
            .pop()
            .expect("node symbol matched by reduce pattern");
        let Some(Symbol::SpineTreeNodes(nodes)) = self.symbols.last_mut() else {
            unreachable!("nodes symbol was checked before pop")
        };
        let Symbol::SpineTreeNode(node) = node else {
            unreachable!("node symbol was checked before pop")
        };
        nodes.push(node);
        true
    }

    fn reduce_node_to_nodes(&mut self) -> bool {
        let Some(Symbol::SpineTreeNode(_)) = self.symbols.last() else {
            return false;
        };
        let Some(Symbol::SpineTreeNode(node)) = self.symbols.pop() else {
            unreachable!("node symbol was checked before pop")
        };
        self.symbols.push(Symbol::SpineTreeNodes(vec![node]));
        true
    }

    fn reduce_root_epoch(&mut self, archive: &SpineArchive) -> Result<bool, SpineError> {
        let Some(compact_idx) = self
            .symbols
            .iter()
            .rposition(|symbol| matches!(symbol, Symbol::Control(ControlSymbol::Compact(..))))
        else {
            return Ok(false);
        };
        let Symbol::Control(ControlSymbol::Compact(memory, next_open_index)) =
            self.symbols[compact_idx].clone()
        else {
            unreachable!("compact symbol was checked before clone")
        };
        let next_open = next_root_open_symbol(archive, &memory, next_open_index)?;
        let Some(boundary_idx) = self.symbols[..compact_idx].iter().rposition(|symbol| {
            matches!(
                symbol,
                Symbol::Control(ControlSymbol::Init(_)) | Symbol::RootEpoches(_)
            )
        }) else {
            return Ok(false);
        };

        let root_epoch = RootEpoch { memory };
        let boundary = self.symbols[boundary_idx].clone();
        match boundary {
            Symbol::Control(ControlSymbol::Init(_)) => {
                self.symbols.truncate(boundary_idx + 1);
                self.symbols.push(Symbol::RootEpoches(vec![root_epoch]));
            }
            Symbol::RootEpoches(mut root_epochs) => {
                self.symbols.truncate(boundary_idx);
                root_epochs.push(root_epoch);
                self.symbols.push(Symbol::RootEpoches(root_epochs));
            }
            _ => unreachable!("root epoch boundary was checked before mutate"),
        }
        self.symbols.push(next_open);
        Ok(true)
    }

    fn render_tree(&self) -> Result<String, SpineError> {
        let mut rows = Vec::<TreeRenderRow>::new();
        collect_tree_render_rows(&self.symbols, &mut rows)?;
        Ok(format_tree_rows(rows))
    }

    fn current_open_meta(&self) -> Result<&TreeMeta, SpineError> {
        self.symbols
            .iter()
            .rev()
            .find_map(|symbol| match symbol {
                Symbol::Control(ControlSymbol::Open(meta)) => Some(meta),
                _ => None,
            })
            .ok_or_else(|| SpineError::InvalidEvent("ParseStack has no live Open".to_string()))
    }

    fn current_root_epoch_id(&self) -> Result<NodeId, SpineError> {
        let current = self.current_open_meta()?.id.clone();
        let root = *current
            .0
            .first()
            .ok_or_else(|| SpineError::InvalidEvent("current node id is empty".to_string()))?;
        Ok(NodeId::root_epoch(root))
    }

    fn next_child_id(&self) -> Result<NodeId, SpineError> {
        let parent = self.current_open_meta()?.id.clone();
        let rows = self.tree_rows()?;
        let child_count = rows
            .iter()
            .filter(|row| row.id.parent().as_ref() == Some(&parent))
            .count();
        let index = u32::try_from(child_count + 1)
            .map_err(|_| SpineError::InvalidEvent("too many child nodes".to_string()))?;
        Ok(parent.child(index))
    }

    fn tree_rows(&self) -> Result<Vec<TreeRenderRow>, SpineError> {
        let mut rows = Vec::<TreeRenderRow>::new();
        collect_tree_render_rows(&self.symbols, &mut rows)?;
        Ok(rows)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TreeRenderRow {
    id: NodeId,
    status: NodeStatus,
    summary: String,
}

fn collect_tree_render_rows(
    symbols: &[Symbol],
    rows: &mut Vec<TreeRenderRow>,
) -> Result<(), SpineError> {
    for symbol in symbols {
        match symbol {
            Symbol::Control(ControlSymbol::Init(_))
            | Symbol::Control(ControlSymbol::Close(_))
            | Symbol::Control(ControlSymbol::Compact(_, _)) => {}
            Symbol::Control(ControlSymbol::Open(meta)) => {
                rows.push(TreeRenderRow {
                    id: meta.id.clone(),
                    status: NodeStatus::Live,
                    summary: meta.summary.clone(),
                });
            }
            Symbol::SpineTreeNode(node) => {
                collect_tree_render_node(node, rows)?;
            }
            Symbol::SpineTreeNodes(nodes) => {
                for node in nodes {
                    collect_tree_render_node(node, rows)?;
                }
            }
            Symbol::RootEpoches(root_epochs) => {
                for root_epoch in root_epochs {
                    rows.push(TreeRenderRow {
                        id: root_epoch.memory.node_id.clone(),
                        status: NodeStatus::Closed,
                        summary: "root".to_string(),
                    });
                }
            }
        }
    }

    let open_positions = rows
        .iter()
        .enumerate()
        .filter_map(|(idx, row)| (row.status == NodeStatus::Live).then_some(idx))
        .collect::<Vec<_>>();
    let Some((&live_idx, ancestors)) = open_positions.split_last() else {
        return Err(SpineError::InvalidEvent(
            "ParseStack tree render has no live cursor".to_string(),
        ));
    };
    for ancestor_idx in ancestors {
        rows[*ancestor_idx].status = NodeStatus::Suspended;
    }
    for row in rows.iter_mut().skip(live_idx + 1) {
        if row.status == NodeStatus::Live {
            row.status = NodeStatus::Closed;
        }
    }
    Ok(())
}

fn collect_tree_render_node(
    node: &SpineTreeNode,
    rows: &mut Vec<TreeRenderRow>,
) -> Result<(), SpineError> {
    match node {
        SpineTreeNode::MsgAsLeafNode { .. } => {}
        SpineTreeNode::SpineTree { meta, children, .. } => {
            rows.push(TreeRenderRow {
                id: meta.id.clone(),
                status: NodeStatus::Closed,
                summary: meta.summary.clone(),
            });
            for child in children {
                collect_tree_render_node(child, rows)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
fn parse_stack_msg_leaf_count(symbols: &[Symbol]) -> usize {
    symbols
        .iter()
        .map(|symbol| match symbol {
            Symbol::SpineTreeNode(node) => spine_tree_node_msg_leaf_count(node),
            Symbol::SpineTreeNodes(nodes) => nodes.iter().map(spine_tree_node_msg_leaf_count).sum(),
            Symbol::Control(_) | Symbol::RootEpoches(_) => 0,
        })
        .sum()
}

#[cfg(test)]
fn spine_tree_node_msg_leaf_count(node: &SpineTreeNode) -> usize {
    match node {
        SpineTreeNode::MsgAsLeafNode { .. } => 1,
        SpineTreeNode::SpineTree { children, .. } => {
            children.iter().map(spine_tree_node_msg_leaf_count).sum()
        }
    }
}

fn format_tree_rows(rows: Vec<TreeRenderRow>) -> String {
    let rows = rows
        .into_iter()
        .map(|row| (row.id.clone(), row))
        .collect::<BTreeMap<_, _>>();
    let mut visible = BTreeSet::<NodeId>::new();
    let Some(cursor) = rows
        .values()
        .find(|row| row.status == NodeStatus::Live)
        .map(|row| row.id.clone())
    else {
        return String::new();
    };

    let mut path = Vec::<NodeId>::new();
    let mut current = Some(cursor.clone());
    while let Some(id) = current {
        path.push(id.clone());
        current = id.parent();
    }
    path.reverse();

    for node_id in &path {
        visible.insert(node_id.clone());
        if node_id.is_root_epoch() {
            for sibling in rows.keys() {
                if sibling.is_root_epoch() && sibling < node_id {
                    visible.insert(sibling.clone());
                }
            }
        }
        if let Some(parent) = node_id.parent() {
            for sibling in rows.keys() {
                if sibling.parent() == Some(parent.clone()) && sibling < node_id {
                    visible.insert(sibling.clone());
                }
            }
        }
    }
    for (id, row) in &rows {
        if id.parent().as_ref() == Some(&cursor) && row.status == NodeStatus::Closed {
            visible.insert(id.clone());
        }
    }

    let mut lines = Vec::new();
    for (id, row) in rows {
        if !visible.contains(&id) {
            continue;
        }
        let marker = match row.status {
            NodeStatus::Live => "Current",
            NodeStatus::Suspended => "Open",
            NodeStatus::Closed => "Done",
        };
        lines.push(format!(
            "{}[{}] {} {}",
            "  ".repeat(id.0.len().saturating_sub(1)),
            id,
            marker,
            row.summary
        ));
    }
    lines.join("\n")
}

#[derive(Clone, Debug)]
struct SpineArchive {
    root: PathBuf,
}

impl SpineArchive {
    fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn node_dir(&self, id: &NodeId) -> PathBuf {
        self.root.join("nodes").join(id.as_path().replace('.', "/"))
    }
}

fn archive_task_tree(
    archive: &SpineArchive,
    meta: &TreeMeta,
    children: &[SpineTreeNode],
    memory: &MemoryRef,
) -> Result<(PathBuf, PathBuf), SpineError> {
    std::fs::create_dir_all(&meta.node_dir)?;
    let memory_path = meta.node_dir.join("Memory.md");
    let trajs_path = meta.node_dir.join("Trajs.md");
    write_archive_file(&memory_path, &render_memory_archive(memory)?)?;
    write_archive_file(&trajs_path, &render_trajs_archive(children))?;
    Ok((
        archive_relative_path(archive, &memory_path),
        archive_relative_path(archive, &trajs_path),
    ))
}

fn archive_relative_path(archive: &SpineArchive, path: &Path) -> PathBuf {
    path.strip_prefix(&archive.root)
        .unwrap_or(path)
        .to_path_buf()
}

fn next_root_open_symbol(
    archive: &SpineArchive,
    memory: &MemoryRef,
    next_open_index: usize,
) -> Result<Symbol, SpineError> {
    let root_index = *memory
        .node_id
        .0
        .first()
        .ok_or_else(|| SpineError::InvalidEvent("root memory node id is empty".to_string()))?;
    let next_id = NodeId::root_epoch(root_index.saturating_add(1)).child(1);
    Ok(Symbol::Control(ControlSymbol::Open(TreeMeta {
        id: next_id.clone(),
        index: next_open_index,
        summary: "root".to_string(),
        node_dir: archive.node_dir(&next_id),
    })))
}

fn write_archive_file(path: &Path, content: &str) -> Result<(), SpineError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if path.exists() {
        let existing = std::fs::read_to_string(path)?;
        if existing == content {
            return Ok(());
        }
        return Err(SpineError::InvalidStore(format!(
            "archive file {} already exists with different content",
            path.display()
        )));
    }
    std::fs::write(path, content)?;
    Ok(())
}

fn render_memory_archive(memory: &MemoryRef) -> Result<String, SpineError> {
    let body = std::fs::read_to_string(&memory.body_path)?;
    let mut out = String::new();
    out.push_str("# Spine Memory Archive\n\n");
    out.push_str(&format!("compact_id: {}\n", memory.compact_id));
    out.push_str(&format!("node_id: {}\n", memory.node_id));
    out.push_str(&format!("body_path: {}\n", memory.body_path.display()));
    out.push_str(&format!("body_hash: {}\n", memory.body_hash));
    out.push_str(&format!(
        "source_raw_range: [{}..{})\n",
        memory.source_raw_range.start, memory.source_raw_range.end
    ));
    out.push_str(&format!(
        "source_context_range: [{}..{})\n",
        memory.source_context_range.start, memory.source_context_range.end
    ));
    out.push_str(&format!(
        "source_token_seq: [{}..{})\n\n",
        memory.source_token_seq.start, memory.source_token_seq.end
    ));
    out.push_str("## Body\n\n");
    out.push_str(&body);
    if !body.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

fn render_trajs_archive(children: &[SpineTreeNode]) -> String {
    let mut out = String::new();
    out.push_str("# Spine Trajs Archive\n\n");
    for child in children {
        render_trajs_node(&mut out, child, 0);
    }
    out
}

fn render_trajs_node(out: &mut String, node: &SpineTreeNode, depth: usize) {
    let indent = "  ".repeat(depth);
    match node {
        SpineTreeNode::MsgAsLeafNode { msg, from_user } => match msg {
            SegRef::ResponseItem {
                raw_ordinal,
                context_index,
            } => {
                out.push_str(&format!(
                    "{indent}- msg raw_ordinal={raw_ordinal} context_index={context_index} from_user={from_user}\n"
                ));
            }
        },
        SpineTreeNode::SpineTree {
            meta,
            children,
            memory_path,
            trajs_path,
            ..
        } => {
            out.push_str(&format!(
                "{indent}- tree id={} index={} summary={} memory_path={} trajs_path={}\n",
                meta.id,
                meta.index,
                meta.summary,
                memory_path.display(),
                trajs_path.display()
            ));
            for child in children {
                render_trajs_node(out, child, depth + 1);
            }
        }
    }
}

fn collect_checkpoint_refs(
    symbols: &[Symbol],
    tree_meta: &mut Vec<CheckpointTreeMeta>,
    memory_refs: &mut Vec<CheckpointMemoryRef>,
    trajs_refs: &mut Vec<CheckpointTrajsRef>,
) {
    for symbol in symbols {
        match symbol {
            Symbol::Control(ControlSymbol::Init(meta))
            | Symbol::Control(ControlSymbol::Open(meta)) => {
                tree_meta.push(checkpoint_tree_meta(meta));
            }
            Symbol::Control(ControlSymbol::Close(memory))
            | Symbol::Control(ControlSymbol::Compact(memory, _)) => {
                memory_refs.push(checkpoint_memory_ref(memory));
            }
            Symbol::SpineTreeNode(node) => {
                collect_checkpoint_node_refs(node, tree_meta, memory_refs, trajs_refs);
            }
            Symbol::SpineTreeNodes(nodes) => {
                for node in nodes {
                    collect_checkpoint_node_refs(node, tree_meta, memory_refs, trajs_refs);
                }
            }
            Symbol::RootEpoches(root_epochs) => {
                for root_epoch in root_epochs {
                    memory_refs.push(checkpoint_memory_ref(&root_epoch.memory));
                }
            }
        }
    }
}

fn collect_checkpoint_node_refs(
    node: &SpineTreeNode,
    tree_meta: &mut Vec<CheckpointTreeMeta>,
    memory_refs: &mut Vec<CheckpointMemoryRef>,
    trajs_refs: &mut Vec<CheckpointTrajsRef>,
) {
    match node {
        SpineTreeNode::MsgAsLeafNode { .. } => {}
        SpineTreeNode::SpineTree {
            memory,
            meta,
            children,
            trajs_path,
            ..
        } => {
            tree_meta.push(checkpoint_tree_meta(meta));
            memory_refs.push(checkpoint_memory_ref(memory));
            trajs_refs.push(CheckpointTrajsRef {
                node_id: meta.id.to_string(),
                trajs_path: trajs_path.display().to_string(),
            });
            for child in children {
                collect_checkpoint_node_refs(child, tree_meta, memory_refs, trajs_refs);
            }
        }
    }
}

fn checkpoint_tree_meta(meta: &TreeMeta) -> CheckpointTreeMeta {
    CheckpointTreeMeta {
        id: meta.id.to_string(),
        index: meta.index,
        summary: meta.summary.clone(),
        node_dir: meta.node_dir.display().to_string(),
    }
}

fn checkpoint_memory_ref(memory: &MemoryRef) -> CheckpointMemoryRef {
    CheckpointMemoryRef {
        compact_id: memory.compact_id.clone(),
        node_id: memory.node_id.to_string(),
        body_path: memory.body_path.display().to_string(),
        body_hash: memory.body_hash.clone(),
        source_raw_start: memory.source_raw_range.start,
        source_raw_end: memory.source_raw_range.end,
        source_context_start: memory.source_context_range.start,
        source_context_end: memory.source_context_range.end,
        source_token_seq_start: memory.source_token_seq.start,
        source_token_seq_end: memory.source_token_seq.end,
    }
}

fn tree_meta(
    archive: &SpineArchive,
    id: NodeId,
    index: u64,
    summary: String,
) -> Result<TreeMeta, SpineError> {
    let index = usize::try_from(index)
        .map_err(|_| SpineError::InvalidEvent("context index overflow".to_string()))?;
    Ok(TreeMeta {
        node_dir: archive.node_dir(&id),
        id,
        index,
        summary,
    })
}

fn memory_ref(
    archive: &SpineArchive,
    compact_id: String,
    node_id: NodeId,
    body_hash: String,
    source_raw_range: Range<u64>,
    source_context_range: Range<usize>,
    source_token_seq: Range<u64>,
) -> MemoryRef {
    MemoryRef {
        body_path: archive.root.join(BODY_DIR).join(format!("{compact_id}.md")),
        compact_id,
        node_id,
        body_hash,
        source_raw_range,
        source_context_range,
        source_token_seq,
    }
}

fn event_to_token(
    event: &LoggedKEvent,
    archive: &SpineArchive,
    mems: &BTreeMap<String, MemRecord>,
    raw_mask: RawMask<'_>,
) -> Result<SpineToken, SpineError> {
    match &event.event {
        KEvent::Init { raw_start } => Ok(SpineToken::Init {
            meta: tree_meta(
                archive,
                NodeId::root_epoch(1),
                *raw_start,
                "root".to_string(),
            )?,
        }),
        KEvent::Msg {
            raw_ordinal,
            context_index,
            from_user,
        } => Ok(SpineToken::Msg {
            seg: SegRef::ResponseItem {
                raw_ordinal: *raw_ordinal,
                context_index: usize::try_from(*context_index)
                    .map_err(|_| SpineError::InvalidEvent("context index overflow".to_string()))?,
            },
            from_user: *from_user,
        }),
        KEvent::Open {
            child,
            index,
            summary,
            ..
        } => Ok(SpineToken::Open {
            meta: tree_meta(archive, child.clone(), *index, summary.clone())?,
        }),
        KEvent::Close { node, .. } => {
            let mem = mems.values().find(|mem| &mem.node == node).ok_or_else(|| {
                SpineError::InvalidEvent(format!("missing memory for close node {node}"))
            })?;
            if !mem.allowed_by(raw_mask)? {
                return Err(SpineError::InvalidEvent(format!(
                    "memory {} does not cover live raw evidence",
                    mem.compact_id
                )));
            }
            Ok(SpineToken::Close {
                memory: memory_ref(
                    archive,
                    mem.compact_id.clone(),
                    mem.node.clone(),
                    mem.body_hash.clone(),
                    mem.raw_start..mem.raw_end,
                    mem.context_start..mem.context_end,
                    event.seq..event.seq + 1,
                ),
            })
        }
        KEvent::RootCompact {
            mem,
            next_open_index,
            ..
        } => {
            let mem = mems.get(mem).ok_or_else(|| {
                SpineError::InvalidEvent("missing memory for root compact".to_string())
            })?;
            if !mem.allowed_by(raw_mask)? {
                return Err(SpineError::InvalidEvent(format!(
                    "memory {} does not cover live raw evidence",
                    mem.compact_id
                )));
            }
            Ok(SpineToken::Compact {
                memory: memory_ref(
                    archive,
                    mem.compact_id.clone(),
                    mem.node.clone(),
                    mem.body_hash.clone(),
                    mem.raw_start..mem.raw_end,
                    mem.context_start..mem.context_end,
                    event.seq..event.seq + 1,
                ),
                next_open_index: usize::try_from(*next_open_index).map_err(|_| {
                    SpineError::InvalidEvent("root open index overflow".to_string())
                })?,
            })
        }
    }
}

fn parse_stack_from_events(
    events: &[LoggedKEvent],
    archive: &SpineArchive,
    mems: &[MemRecord],
    raw_mask: RawMask<'_>,
) -> Result<ParseStack, SpineError> {
    let mems = mems
        .iter()
        .cloned()
        .map(|mem| (mem.compact_id.clone(), mem))
        .collect::<BTreeMap<_, _>>();
    let mut ps = ParseStack::new();
    for event in events {
        if !event.allowed_by(raw_mask)? {
            continue;
        }
        ps.shift(event_to_token(event, archive, &mems, raw_mask)?, archive)?;
    }
    Ok(ps)
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
                target.append_event(&event.event)?;
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

    fn checkpoint_dir(&self) -> PathBuf {
        self.root.join(CHECKPOINT_DIR)
    }

    fn checkpoint_path(&self, raw_ordinal: u64) -> PathBuf {
        self.checkpoint_dir()
            .join(format!("pre-user-{raw_ordinal:020}.json"))
    }

    fn append_event(&self, event: &KEvent) -> Result<u64, SpineError> {
        let seq = self.next_event_seq()?;
        append_json_line(
            &self.tree_path(),
            &LoggedKEvent {
                seq,
                event: event.clone(),
            },
        )?;
        Ok(seq)
    }

    fn append_mem(&self, mem: &MemRecord) -> Result<(), SpineError> {
        append_json_line(&self.mem_path(), mem)
    }

    fn events(&self) -> Result<Vec<LoggedKEvent>, SpineError> {
        read_json_lines(&self.tree_path())
    }

    #[cfg(test)]
    fn events_for_test(&self) -> Result<Vec<LoggedKEvent>, SpineError> {
        self.events()
    }

    #[cfg(test)]
    fn checkpoint_for_test(&self, raw_ordinal: u64) -> Result<SpineCheckpoint, SpineError> {
        read_json_file(&self.checkpoint_path(raw_ordinal))
    }

    fn next_event_seq(&self) -> Result<u64, SpineError> {
        if !self.tree_path().exists() {
            return Ok(0);
        }
        Ok(self
            .events()?
            .last()
            .map(|event| event.seq + 1)
            .unwrap_or(0))
    }

    fn mems(&self) -> Result<Vec<MemRecord>, SpineError> {
        if !self.mem_path().exists() {
            return Ok(Vec::new());
        }
        read_json_lines(&self.mem_path())
    }

    fn checkpoints(&self) -> Result<Vec<SpineCheckpoint>, SpineError> {
        let dir = self.checkpoint_dir();
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut paths = std::fs::read_dir(&dir)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<Vec<_>, _>>()?;
        paths.sort();
        paths
            .into_iter()
            .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
            .map(|path| read_json_file(&path))
            .collect()
    }

    fn write_checkpoint(&self, checkpoint: &SpineCheckpoint) -> Result<(), SpineError> {
        let path = self.checkpoint_path(checkpoint.raw_ordinal);
        write_json_file_if_unchanged(&path, checkpoint)
    }

    fn rollback_checkpoint(
        &self,
        rollback_cuts: &[usize],
    ) -> Result<Option<SpineCheckpoint>, SpineError> {
        let Some(cut) = rollback_cuts.iter().min().copied() else {
            return Ok(None);
        };
        let cut = u64::try_from(cut)
            .map_err(|_| SpineError::InvalidEvent("rollback cut overflow".to_string()))?;
        self.checkpoints()?
            .into_iter()
            .find(|checkpoint| checkpoint.raw_ordinal == cut)
            .map(Some)
            .ok_or_else(|| {
                SpineError::InvalidStore(format!(
                    "missing spine rollback checkpoint before raw ordinal {cut}"
                ))
            })
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

impl LoggedKEvent {
    fn allowed_by(&self, raw_mask: RawMask<'_>) -> Result<bool, SpineError> {
        self.event.allowed_by(raw_mask)
    }
}

impl KEvent {
    fn allowed_by(&self, raw_mask: RawMask<'_>) -> Result<bool, SpineError> {
        match self {
            KEvent::Init { .. } => Ok(true),
            KEvent::Msg { raw_ordinal, .. } => raw_mask.raw_index_live(*raw_ordinal),
            KEvent::Open {
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
            KEvent::Close { boundary, .. } => raw_mask.boundary_live(*boundary),
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
            MemKind::Suffix => raw_mask.span_live(self.raw_start, self.raw_end),
            MemKind::RootEpoch => self
                .raw_live_hash
                .as_deref()
                .map(|hash| raw_mask.prefix_hash_matches(self.raw_end, hash))
                .unwrap_or(Ok(false)),
        }
    }
}

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
        suffix_start: usize,
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
        let raw_mask = RawMask::new(&raw_live);
        let archive = SpineArchive::new(store.root.clone());
        let parse_stack = parse_stack_from_events(&events, &archive, &mems, raw_mask)?;
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
        let mem_map = mems
            .iter()
            .cloned()
            .map(|mem| (mem.compact_id.clone(), mem))
            .collect::<BTreeMap<_, _>>();
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

        let mut parse_stack = checkpoint.parse_stack.clone();
        let raw_mask = RawMask::new(&raw_live);
        for event in events
            .iter()
            .filter(|event| event.seq >= checkpoint.token_seq)
        {
            if !event.allowed_by(raw_mask)? {
                continue;
            }
            parse_stack.shift(
                event_to_token(event, &archive, &mem_map, raw_mask)?,
                &archive,
            )?;
        }
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
        let checkpoint = self.build_checkpoint(rollout_path, raw_ordinal, context)?;
        self.store.write_checkpoint(&checkpoint)
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

    fn build_checkpoint(
        &self,
        rollout_path: &Path,
        raw_ordinal: u64,
        context: &[ResponseItem],
    ) -> Result<SpineCheckpoint, SpineError> {
        let raw_ordinal_usize = usize::try_from(raw_ordinal)
            .map_err(|_| SpineError::InvalidEvent("checkpoint raw ordinal overflow".to_string()))?;
        if raw_ordinal_usize > self.raw_live.len() {
            return Err(SpineError::InvalidEvent(
                "checkpoint raw ordinal exceeds raw boundary".to_string(),
            ));
        }
        let mut tree_meta = Vec::new();
        let mut memory_refs = Vec::new();
        let mut trajs_refs = Vec::new();
        collect_checkpoint_refs(
            &self.parse_stack.symbols,
            &mut tree_meta,
            &mut memory_refs,
            &mut trajs_refs,
        );
        Ok(SpineCheckpoint {
            version: CHECKPOINT_VERSION,
            checkpoint_id: format!("pre-user-{raw_ordinal:020}"),
            rollout_path: rollout_path.display().to_string(),
            raw_ordinal,
            token_seq: self.store.next_event_seq()?,
            raw_live_hash: hash_raw_live(&self.raw_live[..raw_ordinal_usize]),
            context_len: context.len(),
            cursor: self.parse_stack.current_open_meta()?.id.to_string(),
            parse_stack: self.parse_stack.clone(),
            parse_stack_symbols: self
                .parse_stack
                .symbols
                .iter()
                .map(|symbol| format!("{symbol:?}"))
                .collect(),
            tree_meta,
            memory_refs,
            trajs_refs,
            h_ps_hash: hash_response_items(context)?,
            context_hash: hash_response_items(context)?,
        })
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
                let suffix_start = pending.index.ok_or_else(|| {
                    SpineError::InvalidEvent("missing spine.open context index".to_string())
                })?;
                SpineCommitKind::Open {
                    suffix_start: usize::try_from(suffix_start).map_err(|_| {
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

fn render_parse_stack_to_context(
    ps: &ParseStack,
    raw_items: &[Option<ResponseItem>],
) -> Result<Vec<ResponseItem>, SpineError> {
    let mut out = Vec::new();
    render_symbols_to_context(&ps.symbols, raw_items, &mut out)?;
    Ok(out)
}

fn render_symbols_to_context(
    symbols: &[Symbol],
    raw_items: &[Option<ResponseItem>],
    out: &mut Vec<ResponseItem>,
) -> Result<(), SpineError> {
    for symbol in symbols {
        match symbol {
            Symbol::Control(ControlSymbol::Init(_))
            | Symbol::Control(ControlSymbol::Open(_))
            | Symbol::Control(ControlSymbol::Close(_))
            | Symbol::Control(ControlSymbol::Compact(_, _)) => {}
            Symbol::SpineTreeNode(node) => render_node_to_context(node, raw_items, out)?,
            Symbol::SpineTreeNodes(nodes) => {
                for node in nodes {
                    render_node_to_context(node, raw_items, out)?;
                }
            }
            Symbol::RootEpoches(root_epochs) => {
                if let Some(root_epoch) = root_epochs.last() {
                    out.push(memory_response_item(&read_memory_ref_body(
                        &root_epoch.memory,
                    )?));
                }
            }
        }
    }
    Ok(())
}

fn render_node_to_context(
    node: &SpineTreeNode,
    raw_items: &[Option<ResponseItem>],
    out: &mut Vec<ResponseItem>,
) -> Result<(), SpineError> {
    match node {
        SpineTreeNode::MsgAsLeafNode {
            msg:
                SegRef::ResponseItem {
                    raw_ordinal,
                    context_index: _,
                },
            ..
        } => {
            let raw_index = usize::try_from(*raw_ordinal)
                .map_err(|_| SpineError::InvalidEvent("raw ordinal overflow".to_string()))?;
            let item = raw_items
                .get(raw_index)
                .and_then(Option::as_ref)
                .ok_or_else(|| {
                    SpineError::InvalidEvent(format!(
                        "missing raw item for visible Msg raw ordinal {raw_ordinal}"
                    ))
                })?;
            out.push(item.clone());
            Ok(())
        }
        SpineTreeNode::SpineTree { memory, .. } => {
            if memory_ref_is_live(memory, raw_items)? {
                out.push(memory_response_item(&read_memory_ref_body(memory)?));
            } else {
                return Err(SpineError::InvalidEvent(format!(
                    "memory {} does not cover live raw evidence",
                    memory.compact_id
                )));
            }
            Ok(())
        }
    }
}

fn read_memory_ref_body(memory: &MemoryRef) -> Result<String, SpineError> {
    let body = std::fs::read_to_string(&memory.body_path)?;
    if sha1_hex(body.as_bytes()) != memory.body_hash {
        return Err(SpineError::InvalidStore(format!(
            "memory body hash mismatch for {}",
            memory.compact_id
        )));
    }
    Ok(body)
}

fn memory_ref_is_live(
    memory: &MemoryRef,
    raw_items: &[Option<ResponseItem>],
) -> Result<bool, SpineError> {
    let start = usize::try_from(memory.source_raw_range.start)
        .map_err(|_| SpineError::InvalidEvent("memory raw start overflow".to_string()))?;
    let end = usize::try_from(memory.source_raw_range.end)
        .map_err(|_| SpineError::InvalidEvent("memory raw end overflow".to_string()))?;
    if start > end || end > raw_items.len() {
        return Ok(false);
    }
    Ok(raw_items[start..end].iter().all(Option::is_some))
}

fn validate_checkpoint(
    checkpoint: &SpineCheckpoint,
    rollout_path: &Path,
    raw_live: &[bool],
    raw_items: &[Option<ResponseItem>],
) -> Result<(), SpineError> {
    if checkpoint.version != CHECKPOINT_VERSION {
        return Err(SpineError::InvalidStore(format!(
            "unsupported spine checkpoint version {}",
            checkpoint.version
        )));
    }
    let end = usize::try_from(checkpoint.raw_ordinal)
        .map_err(|_| SpineError::InvalidEvent("checkpoint raw ordinal overflow".to_string()))?;
    if end > raw_live.len() || end > raw_items.len() {
        return Err(SpineError::InvalidStore(format!(
            "spine checkpoint raw boundary exceeds rollout for {}",
            checkpoint.checkpoint_id
        )));
    }
    if checkpoint.rollout_path != rollout_path.display().to_string() {
        return Err(SpineError::InvalidStore(format!(
            "spine checkpoint rollout identity mismatch for {}",
            checkpoint.checkpoint_id
        )));
    }
    if checkpoint.raw_live_hash != hash_raw_live(&raw_live[..end]) {
        return Err(SpineError::InvalidStore(format!(
            "spine checkpoint raw boundary hash mismatch for {}",
            checkpoint.checkpoint_id
        )));
    }
    let materialized = render_parse_stack_to_context(&checkpoint.parse_stack, &raw_items[..end])?;
    if materialized.len() != checkpoint.context_len {
        return Err(SpineError::InvalidStore(format!(
            "spine checkpoint context_len mismatch for {}",
            checkpoint.checkpoint_id
        )));
    }
    let hash = hash_response_items(&materialized)?;
    if hash != checkpoint.h_ps_hash || hash != checkpoint.context_hash {
        return Err(SpineError::InvalidStore(format!(
            "spine checkpoint h(PS) hash mismatch for {}",
            checkpoint.checkpoint_id
        )));
    }
    Ok(())
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

pub(crate) fn is_user_message(item: &ResponseItem) -> bool {
    matches!(item, ResponseItem::Message { role, .. } if role == "user")
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

fn write_json_file_if_unchanged<T: Serialize>(path: &Path, value: &T) -> Result<(), SpineError> {
    let content = serde_json::to_string_pretty(value)? + "\n";
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if path.exists() {
        let existing = std::fs::read_to_string(path)?;
        if existing == content {
            return Ok(());
        }
        return Err(SpineError::InvalidStore(format!(
            "checkpoint file {} already exists with different content",
            path.display()
        )));
    }
    std::fs::write(path, content)?;
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

fn hash_response_items(items: &[ResponseItem]) -> Result<String, SpineError> {
    let bytes = serde_json::to_vec(items)?;
    Ok(sha1_hex(&bytes))
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

    fn logged_events(runtime: &SpineRuntime) -> Vec<LoggedKEvent> {
        runtime.store.events_for_test().expect("events")
    }

    fn event_log(runtime: &SpineRuntime) -> Vec<KEvent> {
        logged_events(runtime)
            .into_iter()
            .map(|event| event.event)
            .collect()
    }

    fn event_log_debug(runtime: &SpineRuntime) -> Vec<String> {
        event_log(runtime)
            .into_iter()
            .map(|event| format!("{event:?}"))
            .collect()
    }

    fn spine_call(name: &str, call_id: &str) -> ResponseItem {
        ResponseItem::FunctionCall {
            id: None,
            name: name.to_string(),
            namespace: Some(SPINE_NAMESPACE.to_string()),
            arguments: "{}".to_string(),
            call_id: call_id.to_string(),
        }
    }

    fn function_output(call_id: &str) -> ResponseItem {
        ResponseItem::FunctionCallOutput {
            call_id: call_id.to_string(),
            output: codex_protocol::models::FunctionCallOutputPayload::from_text("ok".to_string()),
        }
    }

    fn compact_body_with_context_range(
        node_id: &str,
        source_context_range: Range<usize>,
    ) -> SpineCloseCompact {
        SpineCloseCompact {
            body: format!("# Spine Memory {node_id}\n\nreal compact body for {node_id}\n"),
            source_context_range,
        }
    }

    fn open_scope(
        runtime: &mut SpineRuntime,
        raw: &mut Vec<Option<ResponseItem>>,
        call_id: &str,
        summary: &str,
    ) {
        let request = spine_call(SPINE_TOOL_OPEN, call_id);
        let request_ordinal = u64::try_from(raw.len()).expect("raw ordinal fits u64");
        raw.push(Some(request.clone()));
        runtime.observe_raw_items(1).expect("record open request");
        runtime
            .observe_context_item(
                request_ordinal,
                usize::try_from(request_ordinal).expect("context index fits usize"),
                &request,
            )
            .expect("observe open request");
        runtime
            .stage_open(call_id.to_string(), summary.to_string())
            .expect("stage open");

        let output = function_output(call_id);
        let output_ordinal = u64::try_from(raw.len()).expect("raw ordinal fits u64");
        raw.push(Some(output.clone()));
        runtime.observe_raw_items(1).expect("record open output");
        runtime
            .observe_context_item(
                output_ordinal,
                usize::try_from(output_ordinal).expect("context index fits usize"),
                &output,
            )
            .expect("observe open output");
        runtime
            .maybe_commit_output(call_id, None)
            .expect("commit open");
    }

    fn append_msg(runtime: &mut SpineRuntime, raw: &mut Vec<Option<ResponseItem>>, text: &str) {
        let item = text_item(text);
        let raw_ordinal = u64::try_from(raw.len()).expect("raw ordinal fits u64");
        raw.push(Some(item.clone()));
        runtime.observe_raw_items(1).expect("record msg");
        runtime
            .observe_context_item(
                raw_ordinal,
                usize::try_from(raw_ordinal).expect("context index fits usize"),
                &item,
            )
            .expect("observe msg");
    }

    fn close_scope(
        runtime: &mut SpineRuntime,
        raw: &mut Vec<Option<ResponseItem>>,
        call_id: &str,
        node_id: &str,
    ) {
        let request = spine_call(SPINE_TOOL_CLOSE, call_id);
        let request_ordinal = u64::try_from(raw.len()).expect("raw ordinal fits u64");
        raw.push(Some(request.clone()));
        runtime.observe_raw_items(1).expect("record close request");
        runtime
            .observe_context_item(
                request_ordinal,
                usize::try_from(request_ordinal).expect("context index fits usize"),
                &request,
            )
            .expect("observe close request");
        runtime
            .stage_close(call_id.to_string(), None)
            .expect("stage close");
        let suffix_start = match runtime
            .pending_commit(call_id)
            .expect("pending close should be readable")
        {
            Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
            other => panic!("expected pending close, got {other:?}"),
        };

        let output = function_output(call_id);
        let output_ordinal = u64::try_from(raw.len()).expect("raw ordinal fits u64");
        raw.push(Some(output.clone()));
        runtime.observe_raw_items(1).expect("record close output");
        runtime
            .observe_context_item(
                output_ordinal,
                usize::try_from(output_ordinal).expect("context index fits usize"),
                &output,
            )
            .expect("observe close output");
        runtime
            .maybe_commit_output(
                call_id,
                Some(compact_body_with_context_range(
                    node_id,
                    suffix_start..raw.len(),
                )),
            )
            .expect("commit close");
    }

    #[test]
    fn ordinary_response_item_shifts_msg() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rollout = rollout_path(&dir);
        let item = text_item("ordinary");
        let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

        runtime.observe_raw_items(1).expect("observe raw");
        runtime
            .observe_context_item(0, 0, &item)
            .expect("observe context item");

        let events = event_log(&runtime);
        assert!(matches!(
            events.as_slice(),
            [
                KEvent::Init { raw_start: 0 },
                KEvent::Open { summary, .. },
                KEvent::Msg {
                    raw_ordinal: 0,
                    context_index: 0,
                    from_user: true,
                }
            ] if summary == "root"
        ));
        assert_eq!(
            runtime.parse_stack().symbols,
            vec![
                Symbol::Control(ControlSymbol::Init(
                    tree_meta(
                        &runtime.archive(),
                        NodeId::root_epoch(1),
                        0,
                        "root".to_string()
                    )
                    .expect("root meta")
                )),
                Symbol::Control(ControlSymbol::Open(
                    tree_meta(
                        &runtime.archive(),
                        NodeId::root_epoch(1).child(1),
                        0,
                        "root".to_string()
                    )
                    .expect("root open meta")
                )),
                Symbol::SpineTreeNodes(vec![SpineTreeNode::MsgAsLeafNode {
                    msg: SegRef::ResponseItem {
                        raw_ordinal: 0,
                        context_index: 0,
                    },
                    from_user: true,
                }]),
            ]
        );
    }

    #[test]
    fn materialize_history_requires_visible_msg_raw_item() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rollout = rollout_path(&dir);
        let item = text_item("ordinary");
        let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

        runtime.observe_raw_items(1).expect("observe raw");
        runtime
            .observe_context_item(0, 0, &item)
            .expect("observe context item");

        let err = runtime
            .materialize_history(&[None])
            .expect_err("h(PS) must render visible Msg from ParseStack, not raw gaps");
        assert!(
            err.to_string()
                .contains("missing raw item for visible Msg raw ordinal 0"),
            "unexpected materialization error: {err}"
        );
    }

    #[test]
    fn spine_open_request_and_output_are_control_carriers_not_persistent_msg() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rollout = rollout_path(&dir);
        let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

        let request = spine_call(SPINE_TOOL_OPEN, "open");
        runtime.observe_raw_items(1).expect("record open request");
        runtime
            .observe_context_item(0, 0, &request)
            .expect("observe open request");
        runtime
            .stage_open("open".to_string(), "child".to_string())
            .expect("stage open");
        let output = function_output("open");
        runtime.observe_raw_items(1).expect("record open output");
        runtime
            .observe_context_item(1, 1, &output)
            .expect("observe open output");
        runtime
            .maybe_commit_output("open", None)
            .expect("commit open");

        let events = event_log(&runtime);
        assert_eq!(events.len(), 3);
        assert!(matches!(events[0], KEvent::Init { raw_start: 0 }));
        assert!(matches!(
            &events[1],
            KEvent::Open {
                boundary: 0,
                summary,
                ..
            } if summary == "root"
        ));
        assert!(matches!(
            &events[2],
            KEvent::Open {
                boundary: 0,
                summary,
                ..
            } if summary == "child"
        ));
        assert!(matches!(
            runtime.parse_stack().symbols.as_slice(),
            [
                Symbol::Control(ControlSymbol::Init(_)),
                Symbol::Control(ControlSymbol::Open(meta)),
                Symbol::Control(ControlSymbol::Open(child)),
            ] if meta.summary == "root"
                && meta.id == NodeId::root_epoch(1).child(1)
                && child.summary == "child"
                && child.id == NodeId::root_epoch(1).child(1).child(1)
                && child.index == 0
        ));
        assert_eq!(
            runtime
                .materialize_history(&[Some(request), Some(output)])
                .expect("materialize history"),
            Vec::<ResponseItem>::new()
        );
    }

    #[test]
    fn duplicate_open_call_id_does_not_create_second_child() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rollout = rollout_path(&dir);
        let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

        let request = spine_call(SPINE_TOOL_OPEN, "dup-open");
        runtime.observe_raw_items(1).expect("record open request");
        runtime
            .observe_context_item(0, 0, &request)
            .expect("observe first open request");

        runtime
            .observe_raw_items(1)
            .expect("record duplicate request");
        let err = runtime
            .observe_context_item(1, 1, &request)
            .expect_err("duplicate open request anchor must fail fast");
        assert!(
            err.to_string()
                .contains("duplicate spine.open request anchor for dup-open"),
            "unexpected duplicate error: {err}"
        );

        runtime
            .stage_open("dup-open".to_string(), "only child".to_string())
            .expect("stage open");
        let output = function_output("dup-open");
        runtime.observe_raw_items(1).expect("record open output");
        runtime
            .observe_context_item(2, 2, &output)
            .expect("observe open output");
        runtime
            .maybe_commit_output("dup-open", None)
            .expect("commit open");
        let events_after_first_commit = event_log(&runtime);
        let event_debug_after_first_commit = event_log_debug(&runtime);
        assert_eq!(
            events_after_first_commit
                .iter()
                .filter(
                    |event| matches!(event, KEvent::Open { summary, .. } if summary == "only child")
                )
                .count(),
            1
        );
        assert_eq!(
            runtime
                .maybe_commit_output("dup-open", None)
                .expect("duplicate output commit should be no-op"),
            None
        );
        assert_eq!(event_log_debug(&runtime), event_debug_after_first_commit);
        assert_eq!(
            runtime.render_tree().expect("render tree"),
            "  [1.1] Open root\n    [1.1.1] Current only child"
        );
    }

    #[test]
    fn clone_for_rollout_fails_closed_when_visible_memory_body_is_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source_rollout = dir.path().join("source.jsonl");
        let target_rollout = dir.path().join("target.jsonl");
        let source = SpineStore::create_for_rollout(&source_rollout).expect("create source store");
        source
            .append_event(&KEvent::Init { raw_start: 0 })
            .expect("append init");
        let mem = MemRecord {
            compact_id: "mem-missing".to_string(),
            kind: MemKind::Suffix,
            node: NodeId::root_epoch(1).child(1),
            raw_start: 0,
            raw_end: 1,
            context_start: 0,
            context_end: 1,
            raw_live_hash: None,
            body_path: "bodies/mem-missing.md".to_string(),
            body_hash: sha1_hex(b"missing body"),
        };
        source.append_mem(&mem).expect("append missing mem ref");

        let err =
            SpineStore::clone_for_rollout_with_raw_live(&source_rollout, &target_rollout, &[true])
                .expect_err("missing visible memory body must fail closed");
        assert!(
            err.to_string().contains("No such file") || err.to_string().contains("os error 2"),
            "unexpected clone error: {err}"
        );
    }

    #[test]
    fn spine_close_output_does_not_shift_msg() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rollout = rollout_path(&dir);
        let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

        runtime.observe_raw_items(1).expect("record open request");
        runtime
            .observe_context_item(0, 0, &spine_call(SPINE_TOOL_OPEN, "open"))
            .expect("observe open request");
        runtime
            .stage_open("open".to_string(), "child".to_string())
            .expect("stage open");
        runtime.observe_raw_items(1).expect("record open output");
        runtime
            .observe_context_item(1, 1, &function_output("open"))
            .expect("observe open output");
        runtime
            .maybe_commit_output("open", None)
            .expect("commit open");

        runtime.observe_raw_items(1).expect("record close request");
        runtime
            .observe_context_item(2, 2, &spine_call(SPINE_TOOL_CLOSE, "close"))
            .expect("observe close request");
        runtime
            .stage_close("close".to_string(), None)
            .expect("stage close");
        let suffix_start = match runtime.pending_commit("close").expect("pending close") {
            Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
            other => panic!("expected pending close, got {other:?}"),
        };
        runtime.observe_raw_items(1).expect("record close output");
        runtime
            .observe_context_item(3, 3, &function_output("close"))
            .expect("observe close output");
        runtime
            .maybe_commit_output(
                "close",
                Some(compact_body_with_context_range("1.1.1", suffix_start..4)),
            )
            .expect("commit close");

        let events = event_log(&runtime);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, KEvent::Msg { .. }))
                .count(),
            0
        );
        assert!(matches!(events.last(), Some(KEvent::Close { .. })));
        let Some(Symbol::SpineTreeNodes(nodes)) = runtime.parse_stack().symbols.last() else {
            panic!("close should reduce task tree into a tree node inside Nodes")
        };
        assert_eq!(nodes.len(), 1);
        let SpineTreeNode::SpineTree {
            meta,
            children,
            memory_path,
            trajs_path,
            ..
        } = &nodes[0]
        else {
            panic!("close should reduce to SpineTree")
        };
        assert_eq!(meta.id, NodeId::root_epoch(1).child(1).child(1));
        assert_eq!(meta.index, 0);
        assert_eq!(meta.summary, "child");
        assert!(children.is_empty());
        assert_eq!(memory_path, &PathBuf::from("nodes/1/1/1/Memory.md"));
        assert_eq!(trajs_path, &PathBuf::from("nodes/1/1/1/Trajs.md"));

        let memory_archive =
            std::fs::read_to_string(runtime.store.root.join(memory_path)).expect("memory archive");
        assert!(memory_archive.contains("compact_id: mem-1-1-1-0-4"));
        assert!(memory_archive.contains("source_context_range: [0..4)"));
        assert!(memory_archive.contains("# Spine Memory 1.1.1"));
        let trajs_archive =
            std::fs::read_to_string(runtime.store.root.join(trajs_path)).expect("trajs archive");
        assert!(!trajs_archive.contains("msg raw_ordinal="));
    }

    #[test]
    fn close_failure_does_not_mutate_parse_stack() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rollout = rollout_path(&dir);
        let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

        runtime.observe_raw_items(1).expect("record open request");
        runtime
            .observe_context_item(0, 0, &spine_call(SPINE_TOOL_OPEN, "open"))
            .expect("observe open request");
        runtime
            .stage_open("open".to_string(), "child".to_string())
            .expect("stage open");
        runtime.observe_raw_items(1).expect("record open output");
        runtime
            .observe_context_item(1, 1, &function_output("open"))
            .expect("observe open output");
        runtime
            .maybe_commit_output("open", None)
            .expect("commit open");
        runtime.observe_raw_items(1).expect("record child raw");
        runtime
            .observe_context_item(2, 2, &text_item("inside"))
            .expect("observe child raw");
        runtime.observe_raw_items(1).expect("record close request");
        runtime
            .observe_context_item(3, 3, &spine_call(SPINE_TOOL_CLOSE, "close"))
            .expect("observe close request");
        runtime
            .stage_close("close".to_string(), None)
            .expect("stage close");
        runtime.observe_raw_items(1).expect("record close output");
        runtime
            .observe_context_item(4, 4, &function_output("close"))
            .expect("observe close output");

        let parse_stack_before = runtime.parse_stack().clone();
        let tree_before = runtime.render_tree().expect("render tree before failure");
        let events_before = event_log_debug(&runtime);
        let mem_count_before = runtime
            .store
            .mems()
            .expect("read mems before failure")
            .len();
        let err = runtime
            .maybe_commit_output("close", None)
            .expect_err("close without compact output must fail");
        assert!(
            err.to_string()
                .contains("spine.close requires a completed suffix compact"),
            "unexpected close failure: {err}"
        );

        assert_eq!(runtime.parse_stack(), &parse_stack_before);
        assert_eq!(
            runtime.render_tree().expect("render tree after failure"),
            tree_before
        );
        assert_eq!(event_log_debug(&runtime), events_before);
        assert_eq!(
            runtime.store.mems().expect("read mems after failure").len(),
            mem_count_before
        );
        assert!(
            runtime
                .pending_commit("close")
                .expect("pending close")
                .is_some()
        );
    }

    #[test]
    fn close_persistence_failure_does_not_mutate_parse_stack() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rollout = rollout_path(&dir);
        let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

        runtime.observe_raw_items(1).expect("record open request");
        runtime
            .observe_context_item(0, 0, &spine_call(SPINE_TOOL_OPEN, "open"))
            .expect("observe open request");
        runtime
            .stage_open("open".to_string(), "child".to_string())
            .expect("stage open");
        runtime.observe_raw_items(1).expect("record open output");
        runtime
            .observe_context_item(1, 1, &function_output("open"))
            .expect("observe open output");
        runtime
            .maybe_commit_output("open", None)
            .expect("commit open");
        runtime.observe_raw_items(1).expect("record child raw");
        runtime
            .observe_context_item(2, 2, &text_item("inside"))
            .expect("observe child raw");
        runtime.observe_raw_items(1).expect("record close request");
        runtime
            .observe_context_item(3, 3, &spine_call(SPINE_TOOL_CLOSE, "close"))
            .expect("observe close request");
        runtime
            .stage_close("close".to_string(), None)
            .expect("stage close");
        let suffix_start = match runtime.pending_commit("close").expect("pending close") {
            Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
            other => panic!("expected pending close, got {other:?}"),
        };
        runtime.observe_raw_items(1).expect("record close output");
        runtime
            .observe_context_item(4, 4, &function_output("close"))
            .expect("observe close output");

        let parse_stack_before = runtime.parse_stack().clone();
        let tree_before = runtime.render_tree().expect("render tree before failure");
        let events_before = event_log_debug(&runtime);
        std::fs::create_dir(runtime.store.mem_path()).expect("poison mem ledger path");

        let err = runtime
            .maybe_commit_output(
                "close",
                Some(compact_body_with_context_range("1.1.1", suffix_start..5)),
            )
            .expect_err("close mem persistence failure must fail");
        assert!(
            err.to_string().contains("Is a directory")
                || err.to_string().contains("os error 21")
                || err.to_string().contains("Permission denied"),
            "unexpected close persistence failure: {err}"
        );

        assert_eq!(runtime.parse_stack(), &parse_stack_before);
        assert_eq!(
            runtime.render_tree().expect("render tree after failure"),
            tree_before
        );
        assert_eq!(event_log_debug(&runtime), events_before);
        assert!(
            runtime
                .pending_commit("close")
                .expect("pending close")
                .is_some()
        );
    }

    #[test]
    fn nested_close_reduces_inner_tree_into_parent_nodes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rollout = rollout_path(&dir);
        let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

        runtime
            .observe_raw_items(1)
            .expect("record outer open request");
        runtime
            .observe_context_item(0, 0, &spine_call(SPINE_TOOL_OPEN, "outer"))
            .expect("observe outer open request");
        runtime
            .stage_open("outer".to_string(), "outer".to_string())
            .expect("stage outer open");
        runtime.observe_raw_items(1).expect("record outer output");
        runtime
            .observe_context_item(1, 1, &function_output("outer"))
            .expect("observe outer output");
        runtime
            .maybe_commit_output("outer", None)
            .expect("commit outer");

        runtime
            .observe_raw_items(1)
            .expect("record inner open request");
        runtime
            .observe_context_item(2, 2, &spine_call(SPINE_TOOL_OPEN, "inner"))
            .expect("observe inner open request");
        runtime
            .stage_open("inner".to_string(), "inner".to_string())
            .expect("stage inner open");
        runtime.observe_raw_items(1).expect("record inner output");
        runtime
            .observe_context_item(3, 3, &function_output("inner"))
            .expect("observe inner output");
        runtime
            .maybe_commit_output("inner", None)
            .expect("commit inner");

        runtime
            .observe_raw_items(1)
            .expect("record inner close request");
        runtime
            .stage_close("close-inner".to_string(), None)
            .expect("stage inner close");
        let inner_suffix_start = match runtime
            .pending_commit("close-inner")
            .expect("pending inner close")
        {
            Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
            other => panic!("expected pending inner close, got {other:?}"),
        };
        runtime
            .observe_raw_items(1)
            .expect("record inner close output");
        runtime
            .observe_context_item(5, 5, &function_output("close-inner"))
            .expect("observe inner close output");
        runtime
            .maybe_commit_output(
                "close-inner",
                Some(compact_body_with_context_range(
                    "1.1.1.1",
                    inner_suffix_start..6,
                )),
            )
            .expect("commit inner close");

        assert!(matches!(
            runtime.parse_stack().symbols.as_slice(),
            [
                Symbol::Control(ControlSymbol::Init(_)),
                Symbol::Control(ControlSymbol::Open(root)),
                Symbol::Control(ControlSymbol::Open(outer)),
                Symbol::SpineTreeNodes(nodes),
            ] if root.id == NodeId::root_epoch(1).child(1)
                && outer.id == NodeId::root_epoch(1).child(1).child(1)
                && matches!(
                    nodes.as_slice(),
                    [SpineTreeNode::SpineTree { meta, .. }]
                        if meta.id == NodeId::root_epoch(1).child(1).child(1).child(1)
                            && meta.summary == "inner"
                )
        ));

        runtime
            .observe_raw_items(1)
            .expect("record outer close request");
        runtime
            .stage_close("close-outer".to_string(), None)
            .expect("stage outer close");
        let outer_suffix_start = match runtime
            .pending_commit("close-outer")
            .expect("pending outer close")
        {
            Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
            other => panic!("expected pending outer close, got {other:?}"),
        };
        runtime
            .observe_raw_items(1)
            .expect("record outer close output");
        runtime
            .observe_context_item(7, 7, &function_output("close-outer"))
            .expect("observe outer close output");
        runtime
            .maybe_commit_output(
                "close-outer",
                Some(compact_body_with_context_range(
                    "1.1.1",
                    outer_suffix_start..8,
                )),
            )
            .expect("commit outer close");

        let Some(Symbol::SpineTreeNodes(root_nodes)) = runtime.parse_stack().symbols.last() else {
            panic!("outer close should reduce to root Nodes")
        };
        assert!(matches!(
            root_nodes.as_slice(),
            [
                SpineTreeNode::SpineTree {
                    meta,
                    children,
                    trajs_path,
                    ..
                }
            ] if meta.id == NodeId::root_epoch(1).child(1).child(1)
                && meta.summary == "outer"
                && matches!(
                    children.as_slice(),
                    [SpineTreeNode::SpineTree { meta: inner, .. }]
                        if inner.summary == "inner"
                )
                && trajs_path == &PathBuf::from("nodes/1/1/1/Trajs.md")
        ));
        let outer_trajs = std::fs::read_to_string(runtime.store.root.join("nodes/1/1/1/Trajs.md"))
            .expect("outer trajs");
        assert!(outer_trajs.contains("summary=inner"));
    }

    #[test]
    fn layer_1_2_4_example_trace_replays_shift_reduce() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rollout = rollout_path(&dir);
        let mut raw = Vec::new();
        let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

        append_msg(&mut runtime, &mut raw, "root work");
        open_scope(&mut runtime, &mut raw, "open-1-1", "scope 1.1");
        append_msg(&mut runtime, &mut raw, "1.1 work");
        close_scope(&mut runtime, &mut raw, "close-1-1", "1.1");
        open_scope(&mut runtime, &mut raw, "open-1-2", "scope 1.2");
        append_msg(&mut runtime, &mut raw, "1.2 work");
        open_scope(&mut runtime, &mut raw, "open-1-2-1", "scope 1.2.1");
        append_msg(&mut runtime, &mut raw, "1.2.1 work");
        close_scope(&mut runtime, &mut raw, "close-1-2-1", "1.2.1");
        open_scope(&mut runtime, &mut raw, "open-1-2-2", "scope 1.2.2");
        append_msg(&mut runtime, &mut raw, "1.2.2 work");
        close_scope(&mut runtime, &mut raw, "close-1-2-2", "1.2.2");
        close_scope(&mut runtime, &mut raw, "close-1-2", "1.2");
        append_msg(&mut runtime, &mut raw, "1.3 work");
        runtime
            .root_compact("root epoch 1 memory".to_string(), raw.len())
            .expect("root compact");
        append_msg(&mut runtime, &mut raw, "2.1 work");

        assert!(matches!(
            runtime.parse_stack().symbols.as_slice(),
            [
                Symbol::Control(ControlSymbol::Init(_)),
                Symbol::RootEpoches(root_epochs),
                Symbol::Control(ControlSymbol::Open(next_root)),
                Symbol::SpineTreeNodes(nodes),
            ] if root_epochs.len() == 1
                && root_epochs[0].memory.node_id == NodeId::root_epoch(1)
                && next_root.id == NodeId::root_epoch(2).child(1)
                && next_root.index == raw.len() - 1
                && matches!(
                    nodes.as_slice(),
                    [
                        SpineTreeNode::MsgAsLeafNode {
                            msg: SegRef::ResponseItem {
                                raw_ordinal,
                                context_index,
                            },
                            ..
                        }
                    ] if *raw_ordinal == u64::try_from(raw.len() - 1).expect("ordinal")
                        && *context_index == raw.len() - 1
                )
        ));

        let replayed = SpineRuntime::load_for_rollout(&rollout, runtime.raw_len)
            .expect("load spine")
            .expect("sidecar exists");
        assert_eq!(
            replayed.parse_stack().symbols,
            runtime.parse_stack().symbols
        );

        let tree = replayed.parse_stack().render_tree().expect("render tree");
        assert!(tree.contains("[1] Done root"), "{tree}");
        assert!(tree.contains("[2.1] Current root"), "{tree}");
        assert!(
            !tree.contains("[1.2.1]") && !tree.contains("[1.2.2]"),
            "closed descendants of a previous root epoch must stay folded: {tree}"
        );

        let materialized = replayed.materialize_history(&raw).expect("materialize");
        assert_eq!(materialized.len(), 2);
        assert!(matches!(
            &materialized[0],
            ResponseItem::Message { content, .. }
                if matches!(
                    content.as_slice(),
                    [ContentItem::InputText { text }]
                        if text.contains("root epoch 1 memory")
                )
        ));
        assert_eq!(materialized[1], text_item("2.1 work"));
    }

    #[test]
    fn fork_clone_rewrites_node_dirs_copies_artifacts_and_isolates_parent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let parent_rollout = dir.path().join("parent.jsonl");
        let child_rollout = dir.path().join("child.jsonl");
        let mut raw = Vec::new();
        let mut parent = SpineRuntime::load_or_create(&parent_rollout, 0).expect("create parent");

        append_msg(&mut parent, &mut raw, "parent root before child");
        open_scope(&mut parent, &mut raw, "open-child", "child scope");
        append_msg(&mut parent, &mut raw, "child work");
        close_scope(&mut parent, &mut raw, "close-child", "1.1.1");
        append_msg(&mut parent, &mut raw, "parent after child");

        let parent_materialized = parent.materialize_history(&raw).expect("parent h(PS)");
        let parent_stack_before_child_work = parent.parse_stack().clone();
        let parent_tree_events_before_child_work = event_log_debug(&parent);
        let parent_root = parent.store.root.clone();

        let raw_live = vec![true; raw.len()];
        SpineStore::clone_for_rollout_with_raw_live(&parent_rollout, &child_rollout, &raw_live)
            .expect("clone sidecar");
        let child = SpineRuntime::load_for_rollout_items(&child_rollout, &raw, &[])
            .expect("load child")
            .expect("child sidecar exists");
        let child_root = child.store.root.clone();

        assert_ne!(child_root, parent_root);
        assert_eq!(
            child.materialize_history(&raw).expect("child h(PS)"),
            parent_materialized,
            "fork child h(PS) must match parent at fork boundary"
        );

        let Some(Symbol::SpineTreeNodes(nodes)) = child.parse_stack().symbols.last() else {
            panic!("fork child should replay parent root nodes");
        };
        let child_meta_dir = match nodes.as_slice() {
            [
                SpineTreeNode::MsgAsLeafNode { .. },
                SpineTreeNode::SpineTree {
                    meta,
                    memory_path,
                    trajs_path,
                    ..
                },
                SpineTreeNode::MsgAsLeafNode { .. },
            ] => {
                assert_eq!(meta.id, NodeId::root_epoch(1).child(1).child(1));
                assert!(meta.node_dir.starts_with(&child_root));
                assert!(!meta.node_dir.starts_with(&parent_root));
                assert_eq!(memory_path, &PathBuf::from("nodes/1/1/1/Memory.md"));
                assert_eq!(trajs_path, &PathBuf::from("nodes/1/1/1/Trajs.md"));
                meta.node_dir.clone()
            }
            other => panic!("unexpected fork child nodes: {other:?}"),
        };
        let child_memory_archive =
            std::fs::read_to_string(child_meta_dir.join("Memory.md")).expect("child Memory.md");
        let child_trajs_archive =
            std::fs::read_to_string(child_meta_dir.join("Trajs.md")).expect("child Trajs.md");
        assert!(child_memory_archive.contains("Spine Memory 1.1.1"));
        assert!(child_trajs_archive.contains("raw_ordinal=3"));
        assert!(child_trajs_archive.contains("context_index=3"));
        assert!(child_meta_dir.join("Memory.md").exists());
        assert!(child_meta_dir.join("Trajs.md").exists());

        let mut child = child;
        open_scope(&mut child, &mut raw, "child-open-only", "child-only scope");
        append_msg(&mut child, &mut raw, "child-only work");
        close_scope(&mut child, &mut raw, "child-close-only", "1.1.2");

        let reloaded_parent = SpineRuntime::load_for_rollout(&parent_rollout, parent.raw_len)
            .expect("reload parent")
            .expect("parent sidecar exists");
        assert_eq!(
            reloaded_parent.parse_stack(),
            &parent_stack_before_child_work
        );
        assert_eq!(
            event_log_debug(&reloaded_parent),
            parent_tree_events_before_child_work
        );
    }

    #[test]
    fn open_close_replay_materializes_closed_child_memory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rollout = rollout_path(&dir);
        let raw = vec![
            Some(text_item("before")),
            Some(spine_call(SPINE_TOOL_OPEN, "open")),
            Some(function_output("open")),
            Some(text_item("inside")),
            Some(spine_call(SPINE_TOOL_CLOSE, "close")),
            Some(function_output("close")),
        ];

        let mut runtime = SpineRuntime::load_or_create(&rollout, 1).expect("create spine");
        runtime
            .observe_context_item(0, 0, &text_item("before"))
            .expect("observe prefix");
        runtime.observe_raw_items(1).expect("record open request");
        runtime
            .observe_context_item(1, 1, &spine_call(SPINE_TOOL_OPEN, "open"))
            .expect("observe open request");
        runtime
            .stage_open("open".to_string(), "child scope".to_string())
            .expect("stage open");
        runtime.observe_raw_items(1).expect("record open output");
        runtime
            .observe_context_item(2, 2, &function_output("open"))
            .expect("observe open output");
        runtime
            .maybe_commit_output("open", None)
            .expect("commit open");
        runtime.observe_raw_items(1).expect("observe child item");
        runtime
            .observe_context_item(3, 3, &text_item("inside"))
            .expect("observe child item");
        runtime.observe_raw_items(1).expect("record close request");
        runtime
            .observe_context_item(4, 4, &spine_call(SPINE_TOOL_CLOSE, "close"))
            .expect("observe close request");
        runtime
            .stage_close("close".to_string(), None)
            .expect("stage close");
        let suffix_start = match runtime.pending_commit("close").expect("pending close") {
            Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
            other => panic!("expected pending close, got {other:?}"),
        };
        runtime.observe_raw_items(1).expect("record close output");
        runtime
            .observe_context_item(5, 5, &function_output("close"))
            .expect("observe close output");
        runtime
            .maybe_commit_output(
                "close",
                Some(compact_body_with_context_range("1.1.1", suffix_start..6)),
            )
            .expect("commit close");

        let replayed = SpineRuntime::load_for_rollout(&rollout, runtime.raw_len)
            .expect("load spine")
            .expect("sidecar exists");
        let tree = replayed.render_tree().expect("render tree");
        assert!(tree.contains("[1.1] Current"));
        assert!(tree.contains("[1.1.1] Done child scope"));

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
                            && text.contains("real compact body for 1.1.1")
            )
        ));
    }

    #[test]
    fn tree_renders_from_parse_stack_without_mutating_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rollout = rollout_path(&dir);
        let mut runtime = SpineRuntime::load_or_create(&rollout, 1).expect("create spine");

        runtime.observe_raw_items(1).expect("record open request");
        runtime
            .observe_context_item(1, 1, &spine_call(SPINE_TOOL_OPEN, "open"))
            .expect("observe open request");
        runtime
            .stage_open("open".to_string(), "child scope".to_string())
            .expect("stage open");
        runtime.observe_raw_items(1).expect("record open output");
        runtime
            .observe_context_item(2, 2, &function_output("open"))
            .expect("observe open output");
        runtime
            .maybe_commit_output("open", None)
            .expect("commit open");
        runtime.observe_raw_items(1).expect("observe child item");
        runtime
            .observe_context_item(3, 3, &text_item("inside"))
            .expect("observe child item");
        runtime.observe_raw_items(1).expect("record close request");
        runtime
            .observe_context_item(4, 4, &spine_call(SPINE_TOOL_CLOSE, "close"))
            .expect("observe close request");
        runtime
            .stage_close("close".to_string(), None)
            .expect("stage close");
        let suffix_start = match runtime.pending_commit("close").expect("pending close") {
            Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
            other => panic!("expected pending close, got {other:?}"),
        };
        runtime.observe_raw_items(1).expect("record close output");
        runtime
            .observe_context_item(5, 5, &function_output("close"))
            .expect("observe close output");
        runtime
            .maybe_commit_output(
                "close",
                Some(compact_body_with_context_range("1.1.1", suffix_start..6)),
            )
            .expect("commit close");

        let replayed = SpineRuntime::load_for_rollout(&rollout, runtime.raw_len)
            .expect("load spine")
            .expect("sidecar exists");
        let before = replayed.parse_stack().clone();
        let tree = replayed.render_tree().expect("render tree");
        assert_eq!(replayed.parse_stack(), &before);
        assert_eq!(
            tree,
            replayed.parse_stack().render_tree().expect("render ps")
        );
        assert!(tree.contains("[1.1] Current root"));
        assert!(tree.contains("[1.1.1] Done child scope"));
    }

    #[test]
    fn materialize_history_renders_from_parse_stack_memory_segments() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rollout = rollout_path(&dir);
        let raw = vec![
            Some(text_item("before")),
            Some(spine_call(SPINE_TOOL_OPEN, "open")),
            Some(function_output("open")),
            Some(text_item("inside")),
            Some(spine_call(SPINE_TOOL_CLOSE, "close")),
            Some(function_output("close")),
        ];

        let mut runtime = SpineRuntime::load_or_create(&rollout, 1).expect("create spine");
        runtime
            .observe_context_item(0, 0, &text_item("before"))
            .expect("observe prefix");
        runtime.observe_raw_items(1).expect("record open request");
        runtime
            .observe_context_item(1, 1, &spine_call(SPINE_TOOL_OPEN, "open"))
            .expect("observe open request");
        runtime
            .stage_open("open".to_string(), "child scope".to_string())
            .expect("stage open");
        runtime.observe_raw_items(1).expect("record open output");
        runtime
            .observe_context_item(2, 2, &function_output("open"))
            .expect("observe open output");
        runtime
            .maybe_commit_output("open", None)
            .expect("commit open");
        runtime.observe_raw_items(1).expect("observe child item");
        runtime
            .observe_context_item(3, 3, &text_item("inside"))
            .expect("observe child item");
        runtime.observe_raw_items(1).expect("record close request");
        runtime
            .observe_context_item(4, 4, &spine_call(SPINE_TOOL_CLOSE, "close"))
            .expect("observe close request");
        runtime
            .stage_close("close".to_string(), None)
            .expect("stage close");
        let suffix_start = match runtime.pending_commit("close").expect("pending close") {
            Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
            other => panic!("expected pending close, got {other:?}"),
        };
        runtime.observe_raw_items(1).expect("record close output");
        runtime
            .observe_context_item(5, 5, &function_output("close"))
            .expect("observe close output");
        runtime
            .maybe_commit_output(
                "close",
                Some(compact_body_with_context_range("1.1.1", suffix_start..6)),
            )
            .expect("commit close");

        let Some(Symbol::SpineTreeNodes(nodes)) = runtime.parse_stack().symbols.last() else {
            panic!("closed child should reduce into ParseStack nodes")
        };
        assert!(nodes.iter().any(|node| matches!(
            node,
            SpineTreeNode::SpineTree { memory, .. }
                if memory.compact_id == "mem-1-1-1-1-6"
                    && memory.source_context_range == (1..6)
                    && memory.source_raw_range == (1..6)
        )));

        let materialized = runtime.materialize_history(&raw).expect("materialize");
        assert_eq!(materialized.len(), 2);
        assert_eq!(materialized[0], text_item("before"));
        assert!(matches!(
            &materialized[1],
            ResponseItem::Message { content, .. }
                if matches!(
                    content.as_slice(),
                    [ContentItem::InputText { text }]
                        if text.contains("Spine Memory 1.1.1")
                            && text.contains("real compact body for 1.1.1")
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
        runtime
            .observe_context_item(0, 0, &text_item("kept"))
            .expect("observe kept item");
        runtime
            .observe_context_item(2, 2, &text_item("after rollback"))
            .expect("observe surviving item");
        let materialized = runtime.materialize_history(&raw).expect("materialize");

        assert_eq!(
            materialized,
            vec![text_item("kept"), text_item("after rollback")]
        );
    }

    #[test]
    fn rollback_keeps_open_when_request_item_survives() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rollout = rollout_path(&dir);
        let raw = vec![
            Some(text_item("before")),
            Some(text_item("open request")),
            None,
        ];

        let mut runtime = SpineRuntime::load_or_create(&rollout, 1).expect("create spine");
        runtime
            .observe_context_item(0, 0, &text_item("before"))
            .expect("observe prefix");
        runtime.observe_raw_items(1).expect("record open request");
        runtime
            .observe_context_item(1, 1, &spine_call(SPINE_TOOL_OPEN, "open"))
            .expect("observe open request");
        runtime
            .stage_open("open".to_string(), "child scope".to_string())
            .expect("stage open");
        runtime.observe_raw_items(1).expect("record open output");
        runtime
            .observe_context_item(2, 2, &function_output("open"))
            .expect("observe open output");
        runtime
            .maybe_commit_output("open", None)
            .expect("commit open");

        let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw, &[])
            .expect("load spine")
            .expect("sidecar exists");
        assert_eq!(
            replayed.render_tree().expect("render tree"),
            "  [1.1] Open root\n    [1.1.1] Current child scope"
        );
        assert_eq!(
            replayed.materialize_history(&raw).expect("materialize"),
            vec![text_item("before")]
        );
    }

    #[test]
    fn rollback_skips_open_when_request_item_is_stale() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rollout = rollout_path(&dir);
        let raw = vec![
            Some(text_item("before")),
            None,
            Some(text_item("open output")),
        ];

        let mut runtime = SpineRuntime::load_or_create(&rollout, 1).expect("create spine");
        runtime
            .observe_context_item(0, 0, &text_item("before"))
            .expect("observe prefix");
        runtime.observe_raw_items(1).expect("record open request");
        runtime
            .observe_context_item(1, 1, &spine_call(SPINE_TOOL_OPEN, "open"))
            .expect("observe open request");
        runtime
            .stage_open("open".to_string(), "child scope".to_string())
            .expect("stage open");
        runtime.observe_raw_items(1).expect("record open output");
        runtime
            .observe_context_item(2, 2, &function_output("open"))
            .expect("observe open output");
        runtime
            .maybe_commit_output("open", None)
            .expect("commit open");

        let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw, &[])
            .expect("load spine")
            .expect("sidecar exists");
        assert_eq!(
            replayed.render_tree().expect("render tree"),
            "  [1.1] Current root"
        );
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
            Some(text_item("open request")),
            Some(function_output("open")),
            None,
            Some(function_output("close")),
        ];

        let mut runtime = SpineRuntime::load_or_create(&rollout, 1).expect("create spine");
        runtime
            .observe_context_item(0, 0, &text_item("before"))
            .expect("observe prefix");
        runtime.observe_raw_items(1).expect("record open request");
        runtime
            .observe_context_item(1, 1, &spine_call(SPINE_TOOL_OPEN, "open"))
            .expect("observe open request");
        runtime
            .stage_open("open".to_string(), "child scope".to_string())
            .expect("stage open");
        runtime.observe_raw_items(1).expect("record open output");
        runtime
            .observe_context_item(2, 2, &function_output("open"))
            .expect("observe open output");
        runtime
            .maybe_commit_output("open", None)
            .expect("commit open");
        runtime
            .observe_raw_items(1)
            .expect("record rolled-back child raw");
        runtime
            .observe_context_item(3, 3, &text_item("rolled back child"))
            .expect("observe rolled-back child raw");
        runtime
            .stage_close("close".to_string(), None)
            .expect("stage close");
        let suffix_start = match runtime.pending_commit("close").expect("pending close") {
            Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
            other => panic!("expected pending close, got {other:?}"),
        };
        runtime.observe_raw_items(1).expect("record close output");
        runtime
            .observe_context_item(4, 4, &function_output("close"))
            .expect("observe close output");
        runtime
            .maybe_commit_output(
                "close",
                Some(compact_body_with_context_range("1.1.1", suffix_start..4)),
            )
            .expect("commit close");

        let err = SpineRuntime::load_for_rollout_items(&rollout, &raw, &[])
            .expect_err("suffix memory spanning a rollback hole must fail closed");
        assert!(
            err.to_string()
                .contains("memory mem-1-1-1-1-5 does not cover live raw evidence"),
            "unexpected materialization error: {err}"
        );
    }

    #[test]
    fn native_compact_shifts_compact_and_new_root_open() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rollout = rollout_path(&dir);

        let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
        runtime.observe_raw_items(2).expect("record raw");
        runtime
            .observe_context_item(0, 0, &text_item("before compact"))
            .expect("observe first context item");
        runtime
            .observe_context_item(1, 1, &text_item("more context"))
            .expect("observe second context item");

        runtime
            .root_compact("root summary".to_string(), 1)
            .expect("compact root");

        let events = event_log(&runtime);
        assert!(matches!(
            events.as_slice(),
            [
                KEvent::Init { .. },
                KEvent::Open { summary, .. },
                KEvent::Msg { raw_ordinal: 0, .. },
                KEvent::Msg { raw_ordinal: 1, .. },
                KEvent::RootCompact {
                    boundary: 2,
                    next_open_index: 1,
                    ..
                },
            ] if summary == "root"
        ));
        assert!(matches!(
            runtime.parse_stack().symbols.as_slice(),
            [
                Symbol::Control(ControlSymbol::Init(_)),
                Symbol::RootEpoches(root_epochs),
                Symbol::Control(ControlSymbol::Open(next_root)),
            ] if root_epochs.len() == 1
                && root_epochs[0].memory.node_id == NodeId::root_epoch(1)
                && root_epochs[0].memory.compact_id == "root-1-2"
                && next_root.id == NodeId::root_epoch(2).child(1)
                && next_root.index == 1
                && next_root.summary == "root"
        ));

        let replayed = SpineRuntime::load_for_rollout(&rollout, runtime.raw_len)
            .expect("load spine")
            .expect("sidecar exists");
        assert_eq!(
            replayed.parse_stack().symbols,
            runtime.parse_stack().symbols
        );
    }

    #[test]
    fn native_compact_failure_leaves_parse_stack_unchanged() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rollout = rollout_path(&dir);

        let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
        runtime.observe_raw_items(1).expect("record raw");
        runtime
            .observe_context_item(0, 0, &text_item("before failed compact"))
            .expect("observe context item");
        let parse_stack_before = runtime.parse_stack().clone();
        let tree_before = runtime.render_tree().expect("render tree before failure");
        let events_before = event_log_debug(&runtime);
        let mem_count_before = runtime
            .store
            .mems()
            .expect("read mems before failure")
            .len();

        let err = runtime
            .root_compact("   \n\t".to_string(), 1)
            .expect_err("empty native compact body must fail closed");
        assert!(
            err.to_string()
                .contains("spine root compact memory body must not be empty"),
            "unexpected empty compact error: {err}"
        );

        assert_eq!(runtime.parse_stack(), &parse_stack_before);
        assert_eq!(
            runtime.render_tree().expect("render tree after failure"),
            tree_before
        );
        assert_eq!(event_log_debug(&runtime), events_before);
        assert_eq!(
            runtime.store.mems().expect("read mems after failure").len(),
            mem_count_before
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
            .root_compact("root summary after rollback".to_string(), 1)
            .expect("compact root");

        let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw_after_rollback, &[])
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

    #[test]
    fn checkpoint_before_user_msg_records_recoverable_fields() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rollout = rollout_path(&dir);
        let context = vec![text_item("kept context")];

        let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
        runtime
            .checkpoint_before_user_msg(&rollout, 0, &context)
            .expect("write checkpoint");
        runtime.observe_raw_items(1).expect("observe raw");
        runtime
            .observe_context_item(0, 0, &text_item("first user"))
            .expect("shift user");

        let checkpoint = runtime
            .store
            .checkpoint_for_test(0)
            .expect("read checkpoint");
        assert_eq!(checkpoint.version, CHECKPOINT_VERSION);
        assert_eq!(checkpoint.checkpoint_id, "pre-user-00000000000000000000");
        assert_eq!(checkpoint.rollout_path, rollout.display().to_string());
        assert_eq!(checkpoint.raw_ordinal, 0);
        assert_eq!(checkpoint.token_seq, 2);
        assert_eq!(checkpoint.raw_live_hash, hash_raw_live(&[]));
        assert_eq!(checkpoint.context_len, 1);
        assert_eq!(checkpoint.cursor, "1.1");
        assert_eq!(
            checkpoint.parse_stack.symbols,
            vec![
                Symbol::Control(ControlSymbol::Init(
                    tree_meta(
                        &runtime.archive(),
                        NodeId::root_epoch(1),
                        0,
                        "root".to_string()
                    )
                    .expect("root meta")
                )),
                Symbol::Control(ControlSymbol::Open(
                    tree_meta(
                        &runtime.archive(),
                        NodeId::root_epoch(1).child(1),
                        0,
                        "root".to_string()
                    )
                    .expect("root open meta")
                )),
            ]
        );
        assert_eq!(checkpoint.tree_meta.len(), 2);
        assert!(checkpoint.memory_refs.is_empty());
        assert!(checkpoint.trajs_refs.is_empty());
        assert_eq!(
            checkpoint.h_ps_hash,
            hash_response_items(&context).expect("hash")
        );
        assert_eq!(checkpoint.context_hash, checkpoint.h_ps_hash);
    }

    #[test]
    fn rollback_uses_pre_user_checkpoint_to_restore_parse_stack() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rollout = rollout_path(&dir);
        let raw_after_rollback = vec![Some(text_item("kept")), None];

        let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
        runtime.observe_raw_items(1).expect("observe kept raw");
        runtime
            .observe_context_item(0, 0, &text_item("kept"))
            .expect("observe kept");
        runtime
            .checkpoint_before_user_msg(&rollout, 1, &[text_item("kept")])
            .expect("write checkpoint");
        runtime
            .observe_raw_items(1)
            .expect("observe rolled-back raw");
        runtime
            .observe_context_item(1, 1, &text_item("rolled back"))
            .expect("observe rolled-back user");

        let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw_after_rollback, &[1])
            .expect("load spine")
            .expect("sidecar exists");

        assert_eq!(
            replayed.parse_stack().symbols,
            vec![
                Symbol::Control(ControlSymbol::Init(
                    tree_meta(
                        &replayed.archive(),
                        NodeId::root_epoch(1),
                        0,
                        "root".to_string()
                    )
                    .expect("root meta")
                )),
                Symbol::Control(ControlSymbol::Open(
                    tree_meta(
                        &replayed.archive(),
                        NodeId::root_epoch(1).child(1),
                        0,
                        "root".to_string()
                    )
                    .expect("root open meta")
                )),
                Symbol::SpineTreeNodes(vec![SpineTreeNode::MsgAsLeafNode {
                    msg: SegRef::ResponseItem {
                        raw_ordinal: 0,
                        context_index: 0,
                    },
                    from_user: true,
                }]),
            ]
        );
        assert_eq!(
            replayed
                .materialize_history(&raw_after_rollback)
                .expect("materialize"),
            vec![text_item("kept")]
        );
    }

    #[test]
    fn rollback_checkpoint_replays_new_live_append_after_cut() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rollout = rollout_path(&dir);
        let raw_after_rollback = vec![
            Some(text_item("kept")),
            None,
            Some(text_item("after rollback")),
        ];

        let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
        runtime.observe_raw_items(1).expect("observe kept raw");
        runtime
            .observe_context_item(0, 0, &text_item("kept"))
            .expect("observe kept");
        runtime
            .checkpoint_before_user_msg(&rollout, 1, &[text_item("kept")])
            .expect("write checkpoint");
        runtime
            .observe_raw_items(1)
            .expect("observe rolled-back raw");
        runtime
            .observe_context_item(1, 1, &text_item("rolled back"))
            .expect("observe rolled-back user");
        runtime.observe_raw_items(1).expect("observe new raw");
        runtime
            .observe_context_item(2, 1, &text_item("after rollback"))
            .expect("observe new user");

        let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw_after_rollback, &[1])
            .expect("load spine")
            .expect("sidecar exists");

        assert_eq!(
            replayed
                .materialize_history(&raw_after_rollback)
                .expect("materialize"),
            vec![text_item("kept"), text_item("after rollback")]
        );
        let Some(Symbol::SpineTreeNodes(nodes)) = replayed.parse_stack().symbols.last() else {
            panic!("expected root nodes after replay")
        };
        assert!(matches!(
            nodes.as_slice(),
            [
                SpineTreeNode::MsgAsLeafNode {
                    msg: SegRef::ResponseItem {
                        raw_ordinal: 0,
                        context_index: 0,
                    },
                    ..
                },
                SpineTreeNode::MsgAsLeafNode {
                    msg: SegRef::ResponseItem {
                        raw_ordinal: 2,
                        context_index: 1,
                    },
                    ..
                },
            ]
        ));
    }

    #[test]
    fn rollback_checkpoint_new_open_reuses_restored_sibling_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rollout = rollout_path(&dir);
        let raw_after_rollback = vec![
            Some(text_item("kept")),
            None,
            Some(spine_call(SPINE_TOOL_OPEN, "new-open")),
            Some(function_output("new-open")),
        ];

        let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
        runtime.observe_raw_items(1).expect("observe kept raw");
        runtime
            .observe_context_item(0, 0, &text_item("kept"))
            .expect("observe kept");
        runtime
            .checkpoint_before_user_msg(&rollout, 1, &[text_item("kept")])
            .expect("write checkpoint");
        runtime
            .observe_raw_items(1)
            .expect("observe rolled-back raw");
        runtime
            .observe_context_item(1, 1, &text_item("rolled back"))
            .expect("observe rolled-back user");
        runtime
            .observe_raw_items(1)
            .expect("observe new open request");
        runtime
            .observe_context_item(2, 1, &spine_call(SPINE_TOOL_OPEN, "new-open"))
            .expect("observe new open request");
        runtime
            .stage_open("new-open".to_string(), "restored sibling".to_string())
            .expect("stage new open");
        runtime
            .observe_raw_items(1)
            .expect("observe new open output");
        runtime
            .observe_context_item(3, 2, &function_output("new-open"))
            .expect("observe new open output");
        runtime
            .maybe_commit_output("new-open", None)
            .expect("commit new open");

        let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw_after_rollback, &[1])
            .expect("load spine")
            .expect("sidecar exists");
        assert_eq!(
            replayed.render_tree().expect("render tree"),
            "  [1.1] Open root\n    [1.1.1] Current restored sibling"
        );
        assert!(matches!(
            replayed.parse_stack().symbols.as_slice(),
            [
                Symbol::Control(ControlSymbol::Init(_)),
                Symbol::Control(ControlSymbol::Open(root)),
                Symbol::SpineTreeNodes(nodes),
                Symbol::Control(ControlSymbol::Open(child)),
            ] if root.id == NodeId::root_epoch(1).child(1)
                && matches!(
                    nodes.as_slice(),
                    [SpineTreeNode::MsgAsLeafNode {
                        msg: SegRef::ResponseItem {
                            raw_ordinal: 0,
                            context_index: 0,
                        },
                        ..
                    }]
                )
                && child.id == NodeId::root_epoch(1).child(1).child(1)
                && child.index == 1
                && child.summary == "restored sibling"
        ));
    }

    #[test]
    fn rollback_without_pre_user_checkpoint_fails_closed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rollout = rollout_path(&dir);
        let raw_after_rollback = vec![Some(text_item("kept")), None, Some(text_item("new turn"))];

        let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
        runtime.observe_raw_items(3).expect("observe raw");
        runtime
            .observe_context_item(0, 0, &text_item("kept"))
            .expect("observe kept");
        runtime
            .observe_context_item(1, 1, &text_item("rolled back"))
            .expect("observe rolled-back user");
        runtime
            .observe_context_item(2, 1, &text_item("new turn"))
            .expect("observe new user");

        let err = SpineRuntime::load_for_rollout_items(&rollout, &raw_after_rollback, &[1])
            .expect_err("rollback without checkpoint must fail closed");
        assert!(
            err.to_string()
                .contains("missing spine rollback checkpoint before raw ordinal 1"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn checkpoint_missing_required_field_fails_closed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rollout = rollout_path(&dir);
        let raw_after_rollback = vec![Some(text_item("kept")), None];

        let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
        runtime.observe_raw_items(1).expect("observe kept raw");
        runtime
            .observe_context_item(0, 0, &text_item("kept"))
            .expect("observe kept");
        runtime
            .checkpoint_before_user_msg(&rollout, 1, &[text_item("kept")])
            .expect("write checkpoint");
        let checkpoint_path = runtime.store.checkpoint_path(1);
        let mut checkpoint = serde_json::to_value(
            runtime
                .store
                .checkpoint_for_test(1)
                .expect("read checkpoint"),
        )
        .expect("checkpoint to json value");
        checkpoint
            .as_object_mut()
            .expect("checkpoint object")
            .remove("parse_stack");
        std::fs::write(
            &checkpoint_path,
            serde_json::to_string_pretty(&checkpoint).expect("serialize checkpoint") + "\n",
        )
        .expect("overwrite checkpoint for missing field test");
        runtime
            .observe_raw_items(1)
            .expect("observe rolled-back raw");
        runtime
            .observe_context_item(1, 1, &text_item("rolled back"))
            .expect("observe rolled-back user");

        let err = SpineRuntime::load_for_rollout_items(&rollout, &raw_after_rollback, &[1])
            .expect_err("checkpoint with missing required field must fail closed");
        assert!(
            err.to_string().contains("missing field `parse_stack`"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn corrupt_checkpoint_hash_fails_closed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rollout = rollout_path(&dir);
        let raw_after_rollback = vec![Some(text_item("kept")), None];

        let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
        runtime.observe_raw_items(1).expect("observe kept raw");
        runtime
            .observe_context_item(0, 0, &text_item("kept"))
            .expect("observe kept");
        runtime
            .checkpoint_before_user_msg(&rollout, 1, &[text_item("kept")])
            .expect("write checkpoint");
        let checkpoint_path = runtime.store.checkpoint_path(1);
        let mut checkpoint = runtime
            .store
            .checkpoint_for_test(1)
            .expect("read checkpoint");
        checkpoint.h_ps_hash = "bad-hash".to_string();
        std::fs::write(
            &checkpoint_path,
            serde_json::to_string_pretty(&checkpoint).expect("serialize checkpoint") + "\n",
        )
        .expect("overwrite checkpoint for corruption test");
        runtime
            .observe_raw_items(1)
            .expect("observe rolled-back raw");
        runtime
            .observe_context_item(1, 1, &text_item("rolled back"))
            .expect("observe rolled-back user");

        let err = SpineRuntime::load_for_rollout_items(&rollout, &raw_after_rollback, &[1])
            .expect_err("corrupt checkpoint must fail closed");
        assert!(
            err.to_string()
                .contains("spine checkpoint h(PS) hash mismatch"),
            "unexpected error: {err}"
        );
    }
}
