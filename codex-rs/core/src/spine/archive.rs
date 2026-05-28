use crate::spine::SpineError;
use crate::spine::model::MemoryRef;
use crate::spine::model::NodeId;
use crate::spine::model::SegRef;
use crate::spine::model::SpineTreeNode;
use crate::spine::model::Symbol;
use crate::spine::model::TreeMeta;
use crate::spine::render::read_memory_ref_body;
use crate::spine::store::BODY_DIR;
use std::ops::Range;
use std::path::Path;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub(super) struct SpineArchive {
    pub(super) root: PathBuf,
}

impl SpineArchive {
    pub(super) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub(super) fn node_dir(&self, id: &NodeId) -> PathBuf {
        self.root.join("nodes").join(id.as_path().replace('.', "/"))
    }
}

pub(super) fn archive_task_tree(
    archive: &SpineArchive,
    meta: &TreeMeta,
    children: &[SpineTreeNode],
    memory: &MemoryRef,
) -> Result<(PathBuf, PathBuf), SpineError> {
    std::fs::create_dir_all(&meta.node_dir)?;
    let memory_path = meta.node_dir.join("Memory.md");
    let trajs_path = meta.node_dir.join("Trajs.md");
    write_archive_file(&memory_path, &render_memory_archive(memory)?)?;
    write_archive_file(&trajs_path, &render_trajs_archive(children)?)?;
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

pub(super) fn next_root_open_symbol(
    archive: &SpineArchive,
    memory: &MemoryRef,
    next_open_index: usize,
    open_input_tokens: Option<i64>,
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

fn render_memory_archive(memory: &MemoryRef) -> Result<String, SpineError> {
    let body = read_memory_ref_body(memory)?;

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
    if let Some(tokens) = memory.memory_output_tokens {
        out.push_str(&format!("memory_output_tokens: {tokens}\n"));
    }
    if memory.open_input_tokens.is_some()
        || memory.close_input_tokens.is_some()
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

pub(super) fn tree_meta(
    archive: &SpineArchive,
    id: NodeId,
    index: u64,
    summary: String,
) -> Result<TreeMeta, SpineError> {
    tree_meta_with_open_input_tokens(archive, id, index, summary, None)
}

pub(super) fn tree_meta_with_open_input_tokens(
    archive: &SpineArchive,
    id: NodeId,
    index: u64,
    summary: String,
    open_input_tokens: Option<i64>,
) -> Result<TreeMeta, SpineError> {
    let index = usize::try_from(index)
        .map_err(|_| SpineError::InvalidEvent("context index overflow".to_string()))?;
    Ok(TreeMeta {
        node_dir: archive.node_dir(&id),
        id,
        index,
        summary,
        open_input_tokens,
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
        memory_output_tokens,
    }
}
