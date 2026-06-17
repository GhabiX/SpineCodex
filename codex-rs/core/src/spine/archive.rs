use crate::spine::SpineError;
use crate::spine::io::sha1_hex;
use crate::spine::model::MemoryRef;
use crate::spine::model::NodeId;
use crate::spine::model::SegRef;
use crate::spine::model::SpineTreeNode;
use crate::spine::model::Symbol;
use crate::spine::model::TreeMeta;
use crate::spine::render::read_memory_ref_body;
use crate::spine::store::BODY_DIR;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ops::Range;
use std::path::Path;
use std::path::PathBuf;
use std::rc::Rc;

#[derive(Clone, Debug)]
pub(super) struct SpineArchive {
    pub(super) root: PathBuf,
    staging: Option<Rc<RefCell<ArchiveStaging>>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct StagedArchiveWrite {
    pub(super) path: PathBuf,
    pub(super) content: String,
}

#[derive(Debug, Default)]
struct ArchiveStaging {
    memory_bodies: BTreeMap<String, String>,
    writes: Vec<StagedArchiveWrite>,
}

impl SpineArchive {
    pub(super) fn new(root: PathBuf) -> Self {
        Self {
            root,
            staging: None,
        }
    }

    pub(super) fn staged_with_memory_body(root: PathBuf, memory_id: String, body: String) -> Self {
        let mut memory_bodies = BTreeMap::new();
        memory_bodies.insert(memory_id, body);
        Self {
            root,
            staging: Some(Rc::new(RefCell::new(ArchiveStaging {
                memory_bodies,
                writes: Vec::new(),
            }))),
        }
    }

    pub(super) fn node_dir(&self, id: &NodeId) -> PathBuf {
        self.root.join("nodes").join(id.as_path().replace('.', "/"))
    }

    pub(super) fn staged_writes(&self) -> Vec<StagedArchiveWrite> {
        self.staging
            .as_ref()
            .map(|staging| staging.borrow().writes.clone())
            .unwrap_or_default()
    }

    fn staged_memory_body(&self, memory_id: &str) -> Option<String> {
        self.staging.as_ref().and_then(|staging| {
            staging
                .borrow()
                .memory_bodies
                .get(memory_id)
                .map(ToString::to_string)
        })
    }

    fn stage_archive_file(&self, path: PathBuf, content: String) -> Result<(), SpineError> {
        let Some(staging) = self.staging.as_ref() else {
            return Err(SpineError::Invariant(
                "cannot stage archive file on non-staged archive".to_string(),
            ));
        };
        let mut staging = staging.borrow_mut();
        if let Some(existing) = staging.writes.iter().find(|write| write.path == path) {
            if existing.content == content {
                return Ok(());
            }
            return Err(SpineError::InvalidStore(format!(
                "staged archive file {} already exists with different content",
                path.display()
            )));
        }
        staging.writes.push(StagedArchiveWrite { path, content });
        Ok(())
    }
}

pub(super) fn archive_task_tree(
    archive: &SpineArchive,
    meta: &TreeMeta,
    children: &[SpineTreeNode],
    memory: &MemoryRef,
) -> Result<(PathBuf, PathBuf), SpineError> {
    let memory_path = meta.node_dir.join("Memory.md");
    let trajs_path = meta.node_dir.join("Trajs.md");
    let memory_body = archive.staged_memory_body(&memory.compact_id);
    let memory_content = render_memory_archive(memory, memory_body.as_deref())?;
    let trajs_content = render_trajs_archive(children)?;
    if archive.staging.is_some() {
        archive.stage_archive_file(memory_path.clone(), memory_content)?;
        archive.stage_archive_file(trajs_path.clone(), trajs_content)?;
    } else {
        std::fs::create_dir_all(&meta.node_dir)?;
        write_archive_file(&memory_path, &memory_content)?;
        write_archive_file(&trajs_path, &trajs_content)?;
    }
    Ok((
        archive_relative_path(archive, &memory_path),
        archive_relative_path(archive, &trajs_path),
    ))
}

pub(super) fn flush_archive_writes(writes: &[StagedArchiveWrite]) -> Result<(), SpineError> {
    for write in writes {
        write_archive_file(&write.path, &write.content)?;
    }
    Ok(())
}

fn archive_relative_path(archive: &SpineArchive, path: &Path) -> PathBuf {
    path.strip_prefix(&archive.root)
        .unwrap_or(path)
        .to_path_buf()
}

pub(super) fn next_root_open_symbol(
    archive: &SpineArchive,
    memory: &MemoryRef,
    next_open_index: usize,
    open_input_tokens: Option<i64>,
    open_context_tokens: Option<i64>,
) -> Result<Symbol, SpineError> {
    let root_index = *memory
        .node_id
        .0
        .first()
        .ok_or_else(|| SpineError::InvalidEvent("root memory node id is empty".to_string()))?;
    let next_id = NodeId::root_epoch(root_index.saturating_add(1)).child(1);
    Ok(Symbol::Control(crate::spine::model::ControlSymbol::Open(
        TreeMeta {
            id: next_id.clone(),
            index: next_open_index,
            summary: "root".to_string(),
            open_input_tokens,
            open_context_tokens,
            open_context_source: open_context_tokens
                .map(|_| crate::spine::model::ContextBaselineSource::RootCompactHandoff),
            node_dir: archive.node_dir(&next_id),
        },
    )))
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

fn render_memory_archive(
    memory: &MemoryRef,
    staged_body: Option<&str>,
) -> Result<String, SpineError> {
    let body = match staged_body {
        Some(body) => {
            let actual_hash = sha1_hex(body.as_bytes());
            if actual_hash != memory.body_hash {
                return Err(SpineError::InvalidStore(format!(
                    "staged memory body hash mismatch for {}",
                    memory.compact_id
                )));
            }
            body.to_string()
        }
        None => read_memory_ref_body(memory)?,
    };

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
        "source_token_seq: [{}..{})\n",
        memory.source_token_seq.start, memory.source_token_seq.end
    ));
    if let Some(tokens) = memory.open_input_tokens {
        out.push_str(&format!("open_input_tokens: {tokens}\n"));
    }
    if let Some(tokens) = memory.close_input_tokens {
        out.push_str(&format!("close_input_tokens: {tokens}\n"));
    }
    if let Some(tokens) = memory.open_context_tokens {
        out.push_str(&format!("open_context_tokens: {tokens}\n"));
    }
    if let Some(tokens) = memory.close_context_tokens {
        out.push_str(&format!("close_context_tokens: {tokens}\n"));
    }
    if let Some(tokens) = memory.closed_source_suffix_tokens {
        out.push_str(&format!("closed_source_suffix_tokens: {tokens}\n"));
    }
    if let Some(tokens) = memory.closed_memory_context_tokens {
        out.push_str(&format!("closed_memory_context_tokens: {tokens}\n"));
    }
    if let Some(source) = memory.open_context_source {
        out.push_str(&format!("open_context_source: {source:?}\n"));
    }
    if let Some(tokens) = memory.memory_output_tokens {
        out.push_str(&format!("memory_output_tokens: {tokens}\n"));
    }
    if memory.open_input_tokens.is_some()
        || memory.close_input_tokens.is_some()
        || memory.open_context_tokens.is_some()
        || memory.close_context_tokens.is_some()
        || memory.closed_source_suffix_tokens.is_some()
        || memory.closed_memory_context_tokens.is_some()
        || memory.open_context_source.is_some()
        || memory.memory_output_tokens.is_some()
    {
        out.push('\n');
    }
    out.push_str("## Body\n\n");
    out.push_str(&body);
    if !body.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

fn render_trajs_archive(children: &[SpineTreeNode]) -> Result<String, SpineError> {
    let mut out = String::new();
    out.push_str("# Spine Trajs Archive\n\n");
    for child in children {
        render_trajs_node(&mut out, child)?;
    }
    Ok(out)
}

fn render_trajs_node(out: &mut String, node: &SpineTreeNode) -> Result<(), SpineError> {
    match node {
        SpineTreeNode::MsgAsLeafNode { msg, from_user } => match msg {
            SegRef::ResponseItem {
                raw_ordinal,
                context_index,
            } => {
                out.push_str(&format!(
                    "- raw raw_ordinal={raw_ordinal} context_index={context_index} from_user={from_user}\n"
                ));
            }
            SegRef::Memory {
                memory_id,
                body_path,
            } => {
                out.push_str(&format!(
                    "- memory compact_id={memory_id} body_path={}\n",
                    body_path.display()
                ));
            }
        },
        SpineTreeNode::ToolCallAsLeafNode { segments } => {
            for segment in segments {
                out.push_str("- toolcall ");
                out.push_str(match segment.kind {
                    crate::spine::model::ToolCallSegmentKind::Request => "request ",
                    crate::spine::model::ToolCallSegmentKind::Response => "response ",
                });
                render_trajs_seg_ref(out, &segment.seg)?;
                out.push('\n');
            }
        }
        SpineTreeNode::SpineTree {
            meta,
            memory,
            memory_path,
            trajs_path,
            ..
        } => {
            out.push_str(&format!(
                "- memory compact_id={} node_id={} index={} summary={} body_path={} memory_path={} trajs_path={}\n",
                memory.compact_id,
                meta.id,
                meta.index,
                meta.summary,
                memory.body_path.display(),
                memory_path.display(),
                trajs_path.display()
            ));
        }
    }
    Ok(())
}

fn render_trajs_seg_ref(out: &mut String, seg: &SegRef) -> Result<(), SpineError> {
    match seg {
        SegRef::ResponseItem {
            raw_ordinal,
            context_index,
        } => {
            out.push_str(&format!(
                "raw_ordinal={raw_ordinal} context_index={context_index}"
            ));
        }
        SegRef::Memory {
            memory_id,
            body_path,
        } => {
            out.push_str(&format!(
                "compact_id={memory_id} body_path={}",
                body_path.display()
            ));
        }
    }
    Ok(())
}

pub(super) fn tree_meta(
    archive: &SpineArchive,
    id: NodeId,
    index: u64,
    summary: String,
) -> Result<TreeMeta, SpineError> {
    tree_meta_with_token_baselines(archive, id, index, summary, None, None, None)
}

pub(super) fn tree_meta_with_token_baselines(
    archive: &SpineArchive,
    id: NodeId,
    index: u64,
    summary: String,
    open_input_tokens: Option<i64>,
    open_context_tokens: Option<i64>,
    open_context_source: Option<crate::spine::model::ContextBaselineSource>,
) -> Result<TreeMeta, SpineError> {
    let index = usize::try_from(index)
        .map_err(|_| SpineError::InvalidEvent("context index overflow".to_string()))?;
    Ok(TreeMeta {
        node_dir: archive.node_dir(&id),
        id,
        index,
        summary,
        open_input_tokens,
        open_context_tokens,
        open_context_source,
    })
}

pub(super) fn memory_ref(
    archive: &SpineArchive,
    compact_id: String,
    node_id: NodeId,
    body_hash: String,
    source_raw_range: Range<u64>,
    source_context_range: Range<usize>,
    source_token_seq: Range<u64>,
    open_input_tokens: Option<i64>,
    close_input_tokens: Option<i64>,
    open_context_tokens: Option<i64>,
    close_context_tokens: Option<i64>,
    closed_source_suffix_tokens: Option<i64>,
    closed_memory_context_tokens: Option<i64>,
    open_context_source: Option<crate::spine::model::ContextBaselineSource>,
    memory_output_tokens: Option<i64>,
) -> MemoryRef {
    MemoryRef {
        body_path: archive.root.join(BODY_DIR).join(format!("{compact_id}.md")),
        compact_id,
        node_id,
        body_hash,
        source_raw_range,
        source_context_range,
        source_token_seq,
        open_input_tokens,
        close_input_tokens,
        open_context_tokens,
        close_context_tokens,
        closed_source_suffix_tokens,
        closed_memory_context_tokens,
        open_context_source,
        memory_output_tokens,
    }
}
