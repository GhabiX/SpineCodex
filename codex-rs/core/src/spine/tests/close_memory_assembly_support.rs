use super::ResponseItem;
use super::SpineCloseMemoryAssembly;
use super::SpineCompactSourceEntryKind;
use super::SpineCompactSourcePlan;
use super::SpineError;
use super::is_real_user_message;
use super::user_message_memory_body;
use crate::spine::io::hash_response_items;

pub(super) fn spine_close_memory_assembly_from_tool_arg(
    node_id: &str,
    source_plan: &SpineCompactSourcePlan,
    node_memory: &str,
) -> Result<SpineCloseMemoryAssembly, SpineError> {
    if source_plan.node_id.to_string() != node_id {
        return Err(SpineError::CompactFailure(format!(
            "spine.close source plan node {} does not match close node {node_id}",
            source_plan.node_id
        )));
    }
    if source_plan.entries.is_empty() {
        return Err(SpineError::CompactFailure(format!(
            "spine.close memory source plan is empty for node {node_id}"
        )));
    }
    let skeleton = SpineMemoryAssemblySkeleton::from_source_plan(node_id, source_plan)?;
    let body = skeleton.assemble(node_memory)?;
    Ok(SpineCloseMemoryAssembly {
        body,
        source_context_range: source_plan.source_context_range.clone(),
        source_raw_range: source_plan.source_raw_range.clone(),
        memory_output_tokens: None,
    })
}

pub(super) fn validate_source_plan_against_history(
    source_plan: &SpineCompactSourcePlan,
    raw_items: &[ResponseItem],
    _close_call_id: &str,
) -> Result<(), SpineError> {
    let covered_len = source_plan
        .entries
        .iter()
        .try_fold(0usize, |total, entry| {
            total
                .checked_add(entry.context_item_count())
                .ok_or_else(|| {
                    SpineError::CompactFailure(
                        "spine.close memory source context length overflow".to_string(),
                    )
                })
        })?;
    let expected_len = source_plan.source_context_range.len();
    if covered_len != expected_len {
        return Err(SpineError::CompactFailure(format!(
            "spine.close memory source covered item count {covered_len} does not match source context range length {expected_len} for [{}..{})",
            source_plan.source_context_range.start, source_plan.source_context_range.end
        )));
    }
    let mut expected_context_index = source_plan.source_context_range.start;
    for (expected_ordinal, entry) in source_plan.entries.iter().enumerate() {
        if entry.source_ordinal != expected_ordinal {
            return Err(SpineError::CompactFailure(format!(
                "spine.close memory source entry ordinal {} does not match expected ordinal {expected_ordinal}",
                entry.source_ordinal
            )));
        }
        if entry.context_index != expected_context_index {
            return Err(SpineError::CompactFailure(format!(
                "spine.close memory source entry ordinal {} has context_index {}, expected {expected_context_index} for contiguous source context range [{}..{})",
                entry.source_ordinal,
                entry.context_index,
                source_plan.source_context_range.start,
                source_plan.source_context_range.end
            )));
        }
        let expected_items = entry.visible_response_items()?;
        let mut host_items = Vec::with_capacity(expected_items.len());
        for offset in 0..expected_items.len() {
            let context_index = entry.context_index.checked_add(offset).ok_or_else(|| {
                SpineError::CompactFailure(
                    "spine.close memory source context index overflow".to_string(),
                )
            })?;
            let Some(host_item) = raw_items.get(context_index) else {
                return Err(SpineError::CompactFailure(format!(
                    "spine.close memory source entry ordinal {} context_index {context_index} exceeds history length {}",
                    entry.source_ordinal,
                    raw_items.len()
                )));
            };
            host_items.push(host_item.clone());
        }
        let host_hash = hash_response_items(&host_items)?;
        if host_items != expected_items || host_hash != entry.source_hash {
            return Err(SpineError::CompactFailure(format!(
                "spine.close memory source entry mismatch at ordinal {} context_index {} source_hash {}",
                entry.source_ordinal, entry.context_index, entry.source_hash
            )));
        }
        expected_context_index = expected_context_index
            .checked_add(entry.context_item_count())
            .ok_or_else(|| {
                SpineError::CompactFailure(
                    "spine.close memory source context index overflow".to_string(),
                )
            })?;
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SpineMemoryAssemblySkeleton {
    node_id: String,
    blocks: Vec<SpineMemoryAssemblyBlock>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SpineMemoryAssemblyBlock {
    UserMessage {
        body: String,
        user_anchor: Option<u64>,
    },
    ChildMemory {
        body: String,
    },
}

impl SpineMemoryAssemblySkeleton {
    pub(super) fn from_source_plan(
        node_id: &str,
        source_plan: &SpineCompactSourcePlan,
    ) -> Result<Self, SpineError> {
        let mut blocks = Vec::new();
        for entry in &source_plan.entries {
            match &entry.kind {
                SpineCompactSourceEntryKind::RawResponseItem {
                    item,
                    from_user: true,
                    user_anchor,
                    ..
                } => {
                    if is_real_user_message(item)
                        && let Some(text) = user_message_memory_body(item)
                    {
                        blocks.push(SpineMemoryAssemblyBlock::UserMessage {
                            body: text,
                            user_anchor: *user_anchor,
                        });
                    }
                }
                SpineCompactSourceEntryKind::RawResponseItem {
                    from_user: false, ..
                } => {}
                SpineCompactSourceEntryKind::ChildMemory { body, .. } => {
                    blocks.push(SpineMemoryAssemblyBlock::ChildMemory { body: body.clone() });
                }
            }
        }
        Ok(Self {
            node_id: node_id.to_string(),
            blocks,
        })
    }

    pub(super) fn assemble(&self, node_memory: &str) -> Result<String, SpineError> {
        let mut body = format!("# Spine Memory {}\n", self.node_id);
        for block in &self.blocks {
            match block {
                SpineMemoryAssemblyBlock::UserMessage {
                    body: text,
                    user_anchor,
                } => {
                    let heading = user_anchor
                        .map(|anchor| format!("## User Message [U{anchor}]"))
                        .unwrap_or_else(|| "## User Message".to_string());
                    push_memory_block(&mut body, &heading, text);
                }
                SpineMemoryAssemblyBlock::ChildMemory { body: child } => {
                    push_memory_block(&mut body, "## Child Memory", child);
                }
            }
        }
        validate_generated_node_memory_body(node_memory)?;
        push_memory_block(&mut body, "## Node Memory", node_memory);
        if !body.ends_with('\n') {
            body.push('\n');
        }
        Ok(body)
    }
}

fn push_memory_block(body: &mut String, heading: &str, block_body: &str) {
    body.push('\n');
    body.push_str(heading);
    body.push('\n');
    body.push_str(block_body.trim_matches('\n'));
    body.push('\n');
}

fn validate_generated_node_memory_body(body: &str) -> Result<(), SpineError> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Err(SpineError::CompactFailure(
            "spine.close memory argument produced empty node memory".to_string(),
        ));
    }
    Ok(())
}
