use crate::spine::CHECKPOINT_VERSION;
use crate::spine::SpineError;
use crate::spine::io::hash_raw_live;
use crate::spine::io::hash_response_items;
use crate::spine::model::ControlSymbol;
use crate::spine::model::MemoryRef;
use crate::spine::model::RootEpoch;
use crate::spine::model::SpineTreeNode;
use crate::spine::model::Symbol;
use crate::spine::model::TreeMeta;
use crate::spine::parse_stack::ParseStack;
use crate::spine::render::render_parse_stack_to_context;
use codex_protocol::models::ResponseItem;
use serde::Deserialize;
use serde::Serialize;
use std::path::Path;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct SpineCheckpoint {
    pub(super) version: u32,
    pub(super) checkpoint_id: String,
    pub(super) rollout_path: String,
    pub(super) raw_ordinal: u64,
    pub(super) token_seq: u64,
    pub(super) raw_live_hash: String,
    pub(super) context_len: usize,
    pub(super) cursor: String,
    pub(super) parse_stack: ParseStack,
    pub(super) parse_stack_symbols: Vec<String>,
    pub(super) tree_meta: Vec<CheckpointTreeMeta>,
    pub(super) memory_refs: Vec<CheckpointMemoryRef>,
    pub(super) trajs_refs: Vec<CheckpointTrajsRef>,
    pub(super) h_ps_hash: String,
}

pub(super) fn build_checkpoint(
    rollout_path: &Path,
    raw_ordinal: u64,
    token_seq: u64,
    raw_live: &[bool],
    parse_stack: &ParseStack,
    context: &[ResponseItem],
) -> Result<SpineCheckpoint, SpineError> {
    let raw_ordinal_usize = usize::try_from(raw_ordinal)
        .map_err(|_| SpineError::InvalidEvent("checkpoint raw ordinal overflow".to_string()))?;
    if raw_ordinal_usize > raw_live.len() {
        return Err(SpineError::InvalidEvent(
            "checkpoint raw ordinal exceeds raw boundary".to_string(),
        ));
    }
    let mut tree_meta = Vec::new();
    let mut memory_refs = Vec::new();
    let mut trajs_refs = Vec::new();
    collect_checkpoint_refs(
        &parse_stack.symbols,
        &mut tree_meta,
        &mut memory_refs,
        &mut trajs_refs,
    );
    Ok(SpineCheckpoint {
        version: CHECKPOINT_VERSION,
        checkpoint_id: format!("pre-user-{raw_ordinal:020}"),
        rollout_path: rollout_path.display().to_string(),
        raw_ordinal,
        token_seq,
        raw_live_hash: hash_raw_live(&raw_live[..raw_ordinal_usize]),
        context_len: context.len(),
        cursor: parse_stack.current_cursor_id()?.to_string(),
        parse_stack: parse_stack.clone(),
        parse_stack_symbols: parse_stack
            .symbols
            .iter()
            .map(|symbol| format!("{symbol:?}"))
            .collect(),
        tree_meta,
        memory_refs,
        trajs_refs,
        h_ps_hash: hash_response_items(context)?,
    })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct CheckpointTreeMeta {
    pub(super) id: String,
    pub(super) index: usize,
    pub(super) summary: String,
    pub(super) node_dir: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct CheckpointMemoryRef {
    pub(super) compact_id: String,
    pub(super) node_id: String,
    pub(super) body_path: String,
    pub(super) body_hash: String,
    pub(super) source_raw_start: u64,
    pub(super) source_raw_end: u64,
    pub(super) source_context_start: usize,
    pub(super) source_context_end: usize,
    pub(super) source_token_seq_start: u64,
    pub(super) source_token_seq_end: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct CheckpointTrajsRef {
    pub(super) node_id: String,
    pub(super) trajs_path: String,
}

pub(super) fn collect_checkpoint_refs(
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
            Symbol::Control(ControlSymbol::End) => {}
            Symbol::Control(ControlSymbol::Close(memory))
            | Symbol::Control(ControlSymbol::Compact(memory, _, _)) => {
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
                for RootEpoch { memory } in root_epochs {
                    memory_refs.push(checkpoint_memory_ref(memory));
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

pub(super) fn validate_checkpoint(
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
    if hash != checkpoint.h_ps_hash {
        return Err(SpineError::InvalidStore(format!(
            "spine checkpoint h(PS) hash mismatch for {}",
            checkpoint.checkpoint_id
        )));
    }
    Ok(())
}
