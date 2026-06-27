use crate::spine::CHECKPOINT_VERSION;
use crate::spine::SpineError;
use crate::spine::io::hash_raw_live;
use crate::spine::io::hash_response_items;
use crate::spine::model::ControlSymbol;
use crate::spine::model::LoggedTrimEvent;
use crate::spine::model::MemoryRef;
use crate::spine::model::RootEpoch;
use crate::spine::model::SpineTreeNode;
use crate::spine::model::Symbol;
use crate::spine::model::TreeMeta;
use crate::spine::parse_stack::ParseStack;
use crate::spine::parser::checkpoint_variable_context_from_parse_stack;
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) pressure_seq_watermark: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) trim_seq_watermark: Option<u64>,
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
    pressure_seq_watermark: Option<u64>,
    trim_seq_watermark: Option<u64>,
    raw_live: &[bool],
    parse_stack: &ParseStack,
    context: &[ResponseItem],
) -> Result<SpineCheckpoint, SpineError> {
    let raw_ordinal_usize = checkpoint_raw_ordinal_usize(raw_ordinal)?;
    if raw_ordinal_usize > raw_live.len() {
        return Err(SpineError::InvalidEvent(
            "checkpoint raw ordinal exceeds raw boundary".to_string(),
        ));
    }
    let checkpoint_refs = collect_checkpoint_refs(&parse_stack.symbols);
    Ok(SpineCheckpoint {
        version: CHECKPOINT_VERSION,
        checkpoint_id: format!("pre-user-{raw_ordinal:020}"),
        rollout_path: rollout_path.display().to_string(),
        raw_ordinal,
        token_seq,
        pressure_seq_watermark,
        trim_seq_watermark,
        raw_live_hash: hash_raw_live(&raw_live[..raw_ordinal_usize]),
        context_len: context.len(),
        cursor: parse_stack.current_cursor_id()?.to_string(),
        parse_stack: parse_stack.clone(),
        parse_stack_symbols: parse_stack_symbol_debug_strings(&parse_stack.symbols),
        tree_meta: checkpoint_refs.tree_meta,
        memory_refs: checkpoint_refs.memory_refs,
        trajs_refs: checkpoint_refs.trajs_refs,
        h_ps_hash: hash_response_items(context)?,
    })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct CheckpointTreeMeta {
    pub(super) id: String,
    pub(super) index: usize,
    pub(super) summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) open_input_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) open_context_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) open_context_source: Option<crate::spine::model::ContextBaselineSource>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) open_input_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) close_input_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) open_context_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) close_context_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) closed_source_suffix_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) closed_memory_context_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) open_context_source: Option<crate::spine::model::ContextBaselineSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) memory_output_tokens: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct CheckpointTrajsRef {
    pub(super) node_id: String,
    pub(super) trajs_path: String,
}

#[derive(Default)]
pub(super) struct CheckpointRefs {
    pub(super) tree_meta: Vec<CheckpointTreeMeta>,
    pub(super) memory_refs: Vec<CheckpointMemoryRef>,
    pub(super) trajs_refs: Vec<CheckpointTrajsRef>,
}

pub(super) fn collect_checkpoint_refs(symbols: &[Symbol]) -> CheckpointRefs {
    let mut refs = CheckpointRefs::default();
    collect_checkpoint_refs_into(symbols, &mut refs);
    refs
}

fn collect_checkpoint_refs_into(symbols: &[Symbol], refs: &mut CheckpointRefs) {
    for symbol in symbols {
        match symbol {
            Symbol::Control(ControlSymbol::Init(meta))
            | Symbol::Control(ControlSymbol::Open(meta)) => {
                refs.tree_meta.push(checkpoint_tree_meta(meta));
            }
            Symbol::Control(ControlSymbol::End) => {}
            Symbol::Control(ControlSymbol::Close(memory))
            | Symbol::Control(ControlSymbol::Compact(memory, _, _, _)) => {
                refs.memory_refs.push(checkpoint_memory_ref(memory));
            }
            Symbol::SpineTreeNode(node) => {
                collect_checkpoint_node_refs(node, refs);
            }
            Symbol::SpineTreeNodes(nodes) => {
                for node in nodes {
                    collect_checkpoint_node_refs(node, refs);
                }
            }
            Symbol::RootEpoches(root_epochs) => {
                for RootEpoch { memory } in root_epochs {
                    refs.memory_refs.push(checkpoint_memory_ref(memory));
                }
            }
        }
    }
}

pub(in crate::spine) fn parse_stack_symbol_debug_strings(symbols: &[Symbol]) -> Vec<String> {
    symbols.iter().map(|symbol| format!("{symbol:?}")).collect()
}

fn collect_checkpoint_node_refs(node: &SpineTreeNode, refs: &mut CheckpointRefs) {
    match node {
        SpineTreeNode::MsgAsLeafNode { .. } | SpineTreeNode::ToolCallAsLeafNode { .. } => {}
        SpineTreeNode::SpineTree {
            memory,
            meta,
            children,
            trajs_path,
            ..
        } => {
            refs.tree_meta.push(checkpoint_tree_meta(meta));
            refs.memory_refs.push(checkpoint_memory_ref(memory));
            refs.trajs_refs.push(CheckpointTrajsRef {
                node_id: meta.id.to_string(),
                trajs_path: trajs_path.display().to_string(),
            });
            for child in children {
                collect_checkpoint_node_refs(child, refs);
            }
        }
    }
}

fn checkpoint_tree_meta(meta: &TreeMeta) -> CheckpointTreeMeta {
    CheckpointTreeMeta {
        id: meta.id.to_string(),
        index: meta.index,
        summary: meta.summary.clone(),
        open_input_tokens: meta.open_input_tokens,
        open_context_tokens: meta.open_context_tokens,
        open_context_source: meta.open_context_source,
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
        open_input_tokens: memory.open_input_tokens,
        close_input_tokens: memory.close_input_tokens,
        open_context_tokens: memory.open_context_tokens,
        close_context_tokens: memory.close_context_tokens,
        closed_source_suffix_tokens: memory.closed_source_suffix_tokens,
        closed_memory_context_tokens: memory.closed_memory_context_tokens,
        open_context_source: memory.open_context_source,
        memory_output_tokens: memory.memory_output_tokens,
    }
}

pub(super) fn validate_checkpoint(
    checkpoint: &SpineCheckpoint,
    rollout_path: &Path,
    raw_live: &[bool],
    raw_items: &[Option<ResponseItem>],
    trim_events: &[LoggedTrimEvent],
) -> Result<(), SpineError> {
    if checkpoint.version != CHECKPOINT_VERSION {
        return Err(SpineError::InvalidStore(format!(
            "unsupported spine checkpoint version {}",
            checkpoint.version
        )));
    }
    let end = checkpoint_raw_ordinal_usize(checkpoint.raw_ordinal)?;
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
    let trim_projection = crate::spine::runtime::trim_projection_from_events_for_checkpoint(
        trim_events,
        &raw_live[..end],
        checkpoint.token_seq,
        checkpoint.trim_seq_watermark,
    )?;
    let variable_context = checkpoint_variable_context_from_parse_stack(
        &checkpoint.parse_stack,
        &raw_items[..end],
        &trim_projection,
    )?;
    if variable_context.len() != checkpoint.context_len {
        return Err(SpineError::InvalidStore(format!(
            "spine checkpoint context_len mismatch for {}",
            checkpoint.checkpoint_id
        )));
    }
    let hash = hash_response_items(&variable_context)?;
    if hash != checkpoint.h_ps_hash {
        return Err(SpineError::InvalidStore(format!(
            "spine checkpoint h(PS) hash mismatch for {}",
            checkpoint.checkpoint_id
        )));
    }
    Ok(())
}

fn checkpoint_raw_ordinal_usize(raw_ordinal: u64) -> Result<usize, SpineError> {
    usize::try_from(raw_ordinal)
        .map_err(|_| SpineError::InvalidEvent("checkpoint raw ordinal overflow".to_string()))
}
