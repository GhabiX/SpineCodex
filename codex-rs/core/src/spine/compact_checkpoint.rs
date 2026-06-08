use crate::spine::CHECKPOINT_VERSION;
use crate::spine::SpineError;
use crate::spine::checkpoint::CheckpointMemoryRef;
use crate::spine::checkpoint::collect_checkpoint_refs;
use crate::spine::io::hash_raw_live;
use crate::spine::io::hash_response_items;
use crate::spine::model::SegRef;
use crate::spine::model::SpineTreeNode;
use crate::spine::model::Symbol;
use crate::spine::parse_stack::ParseStack;
use codex_protocol::models::ResponseItem;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct SpineCompactCheckpoint {
    pub(super) version: u32,
    pub(super) rollout_path: String,
    pub(super) raw_boundary: u64,
    pub(super) token_seq: u64,
    pub(super) raw_live_hash: String,
    pub(super) context_len: usize,
    pub(super) h_ps_hash: String,
    pub(super) replacement_history_hash: String,
    pub(super) response_item_refs: Vec<CompactCheckpointResponseItemRef>,
    pub(super) memory_item_refs: Vec<CompactCheckpointMemoryItemRef>,
    pub(super) memory_refs: Vec<CheckpointMemoryRef>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct CompactCheckpointResponseItemRef {
    pub(super) raw_ordinal: u64,
    pub(super) context_index: usize,
    pub(super) item_hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct CompactCheckpointMemoryItemRef {
    pub(super) compact_id: String,
    pub(super) context_index: usize,
    pub(super) item_hash: String,
}

pub(super) fn build_compact_checkpoint(
    rollout_path: &Path,
    raw_boundary: u64,
    token_seq: u64,
    raw_live: &[bool],
    raw_items: &[Option<ResponseItem>],
    parse_stack: &ParseStack,
    context: &[ResponseItem],
    replacement_history: &[ResponseItem],
) -> Result<SpineCompactCheckpoint, SpineError> {
    let raw_boundary_usize = usize::try_from(raw_boundary)
        .map_err(|_| SpineError::InvalidEvent("compact raw boundary overflow".to_string()))?;
    if raw_boundary_usize > raw_live.len() {
        return Err(SpineError::InvalidEvent(
            "compact raw boundary exceeds raw live length".to_string(),
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
    let (response_item_refs, memory_item_refs) =
        collect_visible_item_refs(&parse_stack.symbols, raw_boundary_usize, raw_items, context)?;
    Ok(SpineCompactCheckpoint {
        version: CHECKPOINT_VERSION,
        rollout_path: rollout_path.display().to_string(),
        raw_boundary,
        token_seq,
        raw_live_hash: hash_raw_live(&raw_live[..raw_boundary_usize]),
        context_len: context.len(),
        h_ps_hash: hash_response_items(context)?,
        replacement_history_hash: hash_response_items(replacement_history)?,
        response_item_refs,
        memory_item_refs,
        memory_refs,
    })
}

pub(super) fn validate_compact_checkpoint(
    checkpoint: &SpineCompactCheckpoint,
    rollout_path: &Path,
    raw_live: &[bool],
    raw_items: &[Option<ResponseItem>],
    replacement_history: &[ResponseItem],
) -> Result<(), SpineError> {
    if checkpoint.version != CHECKPOINT_VERSION {
        return Err(SpineError::InvalidStore(format!(
            "unsupported spine compact checkpoint version {}",
            checkpoint.version
        )));
    }
    let end = usize::try_from(checkpoint.raw_boundary)
        .map_err(|_| SpineError::InvalidEvent("compact raw boundary overflow".to_string()))?;
    if end > raw_live.len() {
        return Err(SpineError::InvalidStore(format!(
            "spine compact checkpoint raw boundary exceeds rollout at {}",
            checkpoint.raw_boundary
        )));
    }
    if checkpoint.rollout_path != rollout_path.display().to_string() {
        return Err(SpineError::InvalidStore(format!(
            "spine compact checkpoint rollout identity mismatch at raw boundary {}",
            checkpoint.raw_boundary
        )));
    }
    if checkpoint.raw_live_hash != hash_raw_live(&raw_live[..end]) {
        return Err(SpineError::InvalidStore(format!(
            "spine compact checkpoint raw boundary hash mismatch at raw boundary {}",
            checkpoint.raw_boundary
        )));
    }
    let replacement_history_hash = hash_response_items(replacement_history)?;
    if checkpoint.replacement_history_hash != replacement_history_hash {
        return Err(SpineError::InvalidStore(format!(
            "spine_jit replacement_history does not match sidecar compact checkpoint at raw boundary {}",
            checkpoint.raw_boundary
        )));
    }
    if checkpoint.h_ps_hash != replacement_history_hash {
        return Err(SpineError::InvalidStore(format!(
            "spine compact checkpoint h(PS) hash mismatch at raw boundary {}",
            checkpoint.raw_boundary
        )));
    }
    validate_response_item_refs(checkpoint, raw_live, raw_items, replacement_history)?;
    Ok(())
}

pub(super) fn compact_checkpoint_replacement_history_hash(
    replacement_history: &[ResponseItem],
) -> Result<String, SpineError> {
    hash_response_items(replacement_history)
}

fn collect_visible_item_refs(
    symbols: &[Symbol],
    raw_boundary: usize,
    raw_items: &[Option<ResponseItem>],
    context: &[ResponseItem],
) -> Result<
    (
        Vec<CompactCheckpointResponseItemRef>,
        Vec<CompactCheckpointMemoryItemRef>,
    ),
    SpineError,
> {
    let mut response_item_refs = Vec::new();
    let mut memory_item_refs = Vec::new();
    let mut next_context_index = 0usize;
    for symbol in symbols {
        collect_visible_item_refs_from_symbol(
            symbol,
            raw_boundary,
            raw_items,
            context,
            &mut next_context_index,
            &mut response_item_refs,
            &mut memory_item_refs,
        )?;
    }
    if next_context_index != context.len() {
        return Err(SpineError::InvalidEvent(format!(
            "compact checkpoint item refs covered {next_context_index} context items but h(PS) has {}",
            context.len()
        )));
    }
    validate_response_item_ref_uniqueness(&response_item_refs)?;
    validate_memory_item_ref_uniqueness(&memory_item_refs)?;
    Ok((response_item_refs, memory_item_refs))
}

fn collect_visible_item_refs_from_symbol(
    symbol: &Symbol,
    raw_boundary: usize,
    raw_items: &[Option<ResponseItem>],
    context: &[ResponseItem],
    next_context_index: &mut usize,
    response_item_refs: &mut Vec<CompactCheckpointResponseItemRef>,
    memory_item_refs: &mut Vec<CompactCheckpointMemoryItemRef>,
) -> Result<(), SpineError> {
    match symbol {
        Symbol::Control(_) => Ok(()),
        Symbol::SpineTreeNode(node) => collect_visible_item_refs_from_node(
            node,
            raw_boundary,
            raw_items,
            context,
            next_context_index,
            response_item_refs,
            memory_item_refs,
        ),
        Symbol::SpineTreeNodes(nodes) => {
            for node in nodes {
                collect_visible_item_refs_from_node(
                    node,
                    raw_boundary,
                    raw_items,
                    context,
                    next_context_index,
                    response_item_refs,
                    memory_item_refs,
                )?;
            }
            Ok(())
        }
        Symbol::RootEpoches(root_epochs) => {
            if let Some(root_epoch) = root_epochs.last() {
                collect_memory_item_ref(
                    &root_epoch.memory.compact_id,
                    context,
                    next_context_index,
                    memory_item_refs,
                )?;
            }
            Ok(())
        }
    }
}

fn collect_visible_item_refs_from_node(
    node: &SpineTreeNode,
    raw_boundary: usize,
    raw_items: &[Option<ResponseItem>],
    context: &[ResponseItem],
    next_context_index: &mut usize,
    response_item_refs: &mut Vec<CompactCheckpointResponseItemRef>,
    memory_item_refs: &mut Vec<CompactCheckpointMemoryItemRef>,
) -> Result<(), SpineError> {
    match node {
        SpineTreeNode::MsgAsLeafNode { msg, .. } => match msg {
            SegRef::ResponseItem { .. } => collect_visible_response_item_ref(
                msg,
                raw_boundary,
                raw_items,
                context,
                next_context_index,
                response_item_refs,
            ),
            SegRef::Memory {
                memory_id,
                body_path: _,
            } => collect_memory_item_ref(memory_id, context, next_context_index, memory_item_refs),
        },
        SpineTreeNode::SpineTree { memory, .. } => collect_memory_item_ref(
            &memory.compact_id,
            context,
            next_context_index,
            memory_item_refs,
        ),
        SpineTreeNode::ToolCallAsLeafNode {
            tool_req,
            tool_resps,
        } => {
            collect_visible_response_item_ref(
                tool_req,
                raw_boundary,
                raw_items,
                context,
                next_context_index,
                response_item_refs,
            )?;
            for tool_resp in tool_resps {
                collect_visible_response_item_ref(
                    tool_resp,
                    raw_boundary,
                    raw_items,
                    context,
                    next_context_index,
                    response_item_refs,
                )?;
            }
            Ok(())
        }
    }
}

fn collect_visible_response_item_ref(
    seg: &SegRef,
    raw_boundary: usize,
    raw_items: &[Option<ResponseItem>],
    context: &[ResponseItem],
    next_context_index: &mut usize,
    response_item_refs: &mut Vec<CompactCheckpointResponseItemRef>,
) -> Result<(), SpineError> {
    let SegRef::ResponseItem {
        raw_ordinal,
        context_index,
    } = seg
    else {
        return Err(SpineError::InvalidEvent(
            "compact checkpoint expected response item SegRef".to_string(),
        ));
    };
    let raw_index = usize::try_from(*raw_ordinal).map_err(|_| {
        SpineError::InvalidEvent("compact checkpoint raw ordinal overflow".to_string())
    })?;
    if *context_index != *next_context_index {
        return Err(SpineError::InvalidEvent(format!(
            "compact checkpoint response item raw ordinal {raw_ordinal} has context index {context_index} but renders at {}",
            *next_context_index
        )));
    }
    if raw_index >= raw_boundary {
        return Err(SpineError::InvalidEvent(format!(
            "compact checkpoint response item raw ordinal {raw_ordinal} is outside raw boundary {raw_boundary}"
        )));
    }
    let raw_item = raw_items
        .get(raw_index)
        .and_then(Option::as_ref)
        .ok_or_else(|| {
            SpineError::InvalidEvent(format!(
                "compact checkpoint response item raw ordinal {raw_ordinal} is not live"
            ))
        })?;
    let context_item = context.get(*next_context_index).ok_or_else(|| {
        SpineError::InvalidEvent(format!(
            "compact checkpoint response item context index {} exceeds h(PS)",
            *next_context_index
        ))
    })?;
    let item_hash = hash_response_item(raw_item)?;
    if item_hash != hash_response_item(context_item)? {
        return Err(SpineError::InvalidEvent(format!(
            "compact checkpoint response item raw ordinal {raw_ordinal} does not match h(PS) context index {context_index}"
        )));
    }
    response_item_refs.push(CompactCheckpointResponseItemRef {
        raw_ordinal: *raw_ordinal,
        context_index: *context_index,
        item_hash,
    });
    *next_context_index += 1;
    Ok(())
}

fn collect_memory_item_ref(
    compact_id: &str,
    context: &[ResponseItem],
    next_context_index: &mut usize,
    refs: &mut Vec<CompactCheckpointMemoryItemRef>,
) -> Result<(), SpineError> {
    let context_item = context.get(*next_context_index).ok_or_else(|| {
        SpineError::InvalidEvent(format!(
            "compact checkpoint memory item context index {} exceeds h(PS)",
            *next_context_index
        ))
    })?;
    refs.push(CompactCheckpointMemoryItemRef {
        compact_id: compact_id.to_string(),
        context_index: *next_context_index,
        item_hash: hash_response_item(context_item)?,
    });
    *next_context_index += 1;
    Ok(())
}

fn validate_response_item_refs(
    checkpoint: &SpineCompactCheckpoint,
    raw_live: &[bool],
    raw_items: &[Option<ResponseItem>],
    replacement_history: &[ResponseItem],
) -> Result<(), SpineError> {
    if checkpoint.context_len != replacement_history.len() {
        return Err(SpineError::InvalidStore(format!(
            "spine compact checkpoint context length {} does not match replacement_history length {} at raw boundary {}",
            checkpoint.context_len,
            replacement_history.len(),
            checkpoint.raw_boundary
        )));
    }
    validate_response_item_ref_uniqueness(&checkpoint.response_item_refs)?;
    validate_memory_item_ref_uniqueness(&checkpoint.memory_item_refs)?;
    let mut coverage: BTreeMap<usize, &'static str> = BTreeMap::new();
    for reference in &checkpoint.response_item_refs {
        let raw_index = usize::try_from(reference.raw_ordinal).map_err(|_| {
            SpineError::InvalidEvent("compact checkpoint raw ordinal overflow".to_string())
        })?;
        if raw_index
            >= usize::try_from(checkpoint.raw_boundary).map_err(|_| {
                SpineError::InvalidEvent("compact raw boundary overflow".to_string())
            })?
        {
            return Err(SpineError::InvalidStore(format!(
                "compact checkpoint response item raw ordinal {} exceeds raw boundary {}",
                reference.raw_ordinal, checkpoint.raw_boundary
            )));
        }
        if !raw_live.get(raw_index).copied().unwrap_or(false) {
            return Err(SpineError::InvalidStore(format!(
                "compact checkpoint response item raw ordinal {} is not live at raw boundary {}",
                reference.raw_ordinal, checkpoint.raw_boundary
            )));
        }
        let raw_item = raw_items
            .get(raw_index)
            .and_then(Option::as_ref)
            .ok_or_else(|| {
                SpineError::InvalidStore(format!(
                    "compact checkpoint response item raw ordinal {} is missing from rollout",
                    reference.raw_ordinal
                ))
            })?;
        let raw_hash = hash_response_item(raw_item)?;
        if reference.item_hash != raw_hash {
            return Err(SpineError::InvalidStore(format!(
                "compact checkpoint response item hash mismatch for raw ordinal {}",
                reference.raw_ordinal
            )));
        }
        let replacement_item = replacement_history
            .get(reference.context_index)
            .ok_or_else(|| {
                SpineError::InvalidStore(format!(
                    "compact checkpoint response item context index {} exceeds replacement_history",
                    reference.context_index
                ))
            })?;
        if raw_hash != hash_response_item(replacement_item)? {
            return Err(SpineError::InvalidStore(format!(
                "compact checkpoint response item raw ordinal {} does not match replacement_history context index {}",
                reference.raw_ordinal, reference.context_index
            )));
        }
        insert_coverage(&mut coverage, reference.context_index, "response item")?;
    }
    cover_memory_item_refs(checkpoint, replacement_history, &mut coverage)?;
    for context_index in 0..replacement_history.len() {
        if !coverage.contains_key(&context_index) {
            return Err(SpineError::InvalidStore(format!(
                "compact checkpoint missing response item or memory proof for replacement_history context index {context_index}"
            )));
        }
    }
    Ok(())
}

fn validate_response_item_ref_uniqueness(
    refs: &[CompactCheckpointResponseItemRef],
) -> Result<(), SpineError> {
    let mut raw_ordinals = BTreeSet::new();
    let mut context_indices = BTreeSet::new();
    for reference in refs {
        if !raw_ordinals.insert(reference.raw_ordinal) {
            return Err(SpineError::InvalidStore(format!(
                "duplicate compact checkpoint response item raw ordinal {}",
                reference.raw_ordinal
            )));
        }
        if !context_indices.insert(reference.context_index) {
            return Err(SpineError::InvalidStore(format!(
                "duplicate compact checkpoint response item context index {}",
                reference.context_index
            )));
        }
    }
    Ok(())
}

fn validate_memory_item_ref_uniqueness(
    refs: &[CompactCheckpointMemoryItemRef],
) -> Result<(), SpineError> {
    let mut compact_ids = BTreeSet::new();
    let mut context_indices = BTreeSet::new();
    for reference in refs {
        if !compact_ids.insert(reference.compact_id.clone()) {
            return Err(SpineError::InvalidStore(format!(
                "duplicate compact checkpoint memory item {}",
                reference.compact_id
            )));
        }
        if !context_indices.insert(reference.context_index) {
            return Err(SpineError::InvalidStore(format!(
                "duplicate compact checkpoint memory item context index {}",
                reference.context_index
            )));
        }
    }
    Ok(())
}

fn cover_memory_item_refs(
    checkpoint: &SpineCompactCheckpoint,
    replacement_history: &[ResponseItem],
    coverage: &mut BTreeMap<usize, &'static str>,
) -> Result<(), SpineError> {
    for reference in &checkpoint.memory_item_refs {
        let matching_memory_refs = checkpoint
            .memory_refs
            .iter()
            .filter(|memory| memory.compact_id == reference.compact_id)
            .count();
        if matching_memory_refs != 1 {
            return Err(SpineError::InvalidStore(format!(
                "compact checkpoint memory item {} does not have exactly one MemoryRef",
                reference.compact_id
            )));
        }
        let replacement_item = replacement_history
            .get(reference.context_index)
            .ok_or_else(|| {
                SpineError::InvalidStore(format!(
                    "compact checkpoint memory item context index {} exceeds replacement_history",
                    reference.context_index
                ))
            })?;
        if reference.item_hash != hash_response_item(replacement_item)? {
            return Err(SpineError::InvalidStore(format!(
                "compact checkpoint memory item {} does not match replacement_history context index {}",
                reference.compact_id, reference.context_index
            )));
        }
        insert_coverage(coverage, reference.context_index, "memory")?;
    }
    Ok(())
}

fn insert_coverage(
    coverage: &mut BTreeMap<usize, &'static str>,
    context_index: usize,
    kind: &'static str,
) -> Result<(), SpineError> {
    if let Some(existing) = coverage.insert(context_index, kind) {
        return Err(SpineError::InvalidStore(format!(
            "ambiguous compact checkpoint {existing}/{kind} proof for replacement_history context index {context_index}"
        )));
    }
    Ok(())
}

fn hash_response_item(item: &ResponseItem) -> Result<String, SpineError> {
    hash_response_items(std::slice::from_ref(item))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spine::model::SegRef;
    use crate::spine::model::SpineTreeNode;
    use crate::spine::model::Symbol;
    use codex_protocol::models::ContentItem;

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
    fn replacement_history_response_item_mapping_checked() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rollout = dir.path().join("rollout.jsonl");
        let first = text_item("first ordinary item");
        let second = text_item("second ordinary item");
        let raw_items = vec![Some(first.clone()), Some(second.clone())];
        let raw_live = vec![true, true];
        let parse_stack = ParseStack {
            symbols: vec![Symbol::SpineTreeNodes(vec![
                SpineTreeNode::MsgAsLeafNode {
                    msg: SegRef::ResponseItem {
                        raw_ordinal: 0,
                        context_index: 0,
                    },
                    from_user: true,
                },
                SpineTreeNode::MsgAsLeafNode {
                    msg: SegRef::ResponseItem {
                        raw_ordinal: 1,
                        context_index: 1,
                    },
                    from_user: true,
                },
            ])],
        };
        let replacement_history = vec![first, second];
        let checkpoint = build_compact_checkpoint(
            &rollout,
            2,
            3,
            &raw_live,
            &raw_items,
            &parse_stack,
            &replacement_history,
            &replacement_history,
        )
        .expect("build checkpoint");

        validate_compact_checkpoint(
            &checkpoint,
            &rollout,
            &raw_live,
            &raw_items,
            &replacement_history,
        )
        .expect("valid response item mapping should pass");

        let mut corrupted = checkpoint;
        corrupted.response_item_refs[0].context_index = 1;
        corrupted.response_item_refs[1].context_index = 0;

        let err = validate_compact_checkpoint(
            &corrupted,
            &rollout,
            &raw_live,
            &raw_items,
            &replacement_history,
        )
        .expect_err("corrupted raw/context mapping must fail closed");
        assert!(
            err.to_string()
                .contains("does not match replacement_history context index"),
            "unexpected checkpoint validation error: {err}"
        );
    }
}
