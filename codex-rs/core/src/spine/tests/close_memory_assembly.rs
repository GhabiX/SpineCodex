#[cfg(test)]
use crate::spine::SpineCloseMemoryAssembly;
#[cfg(test)]
use crate::spine::SpineCompactSourceEntryKind;
#[cfg(test)]
use crate::spine::SpineCompactSourcePlan;
#[cfg(test)]
use crate::spine::SpineError;
#[cfg(test)]
use crate::spine::is_real_user_message;
#[cfg(test)]
use crate::spine::user_message_memory_body;
#[cfg(test)]
use codex_protocol::models::ContentItem;
#[cfg(test)]
use codex_protocol::models::ResponseItem;

#[cfg(test)]
fn spine_close_memory_assembly_from_tool_arg(
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

#[cfg(test)]
fn validate_source_plan_against_history(
    source_plan: &SpineCompactSourcePlan,
    raw_items: &[ResponseItem],
    _close_call_id: &str,
) -> Result<(), SpineError> {
    let expected_len = source_plan.source_context_range.len();
    if source_plan.entries.len() != expected_len {
        return Err(SpineError::CompactFailure(format!(
            "spine.close memory source entry count {} does not match source context range length {expected_len} for [{}..{})",
            source_plan.entries.len(),
            source_plan.source_context_range.start,
            source_plan.source_context_range.end
        )));
    }
    for (expected_ordinal, entry) in source_plan.entries.iter().enumerate() {
        if entry.source_ordinal != expected_ordinal {
            return Err(SpineError::CompactFailure(format!(
                "spine.close memory source entry ordinal {} does not match expected ordinal {expected_ordinal}",
                entry.source_ordinal
            )));
        }
        let expected_context_index = source_plan
            .source_context_range
            .start
            .checked_add(expected_ordinal)
            .ok_or_else(|| {
                SpineError::CompactFailure(
                    "spine.close memory source context index overflow".to_string(),
                )
            })?;
        if entry.context_index != expected_context_index {
            return Err(SpineError::CompactFailure(format!(
                "spine.close memory source entry ordinal {} has context_index {}, expected {expected_context_index} for contiguous source context range [{}..{})",
                entry.source_ordinal,
                entry.context_index,
                source_plan.source_context_range.start,
                source_plan.source_context_range.end
            )));
        }
        let Some(host_item) = raw_items.get(entry.context_index) else {
            return Err(SpineError::CompactFailure(format!(
                "spine.close memory source entry ordinal {} context_index {} exceeds history length {}",
                entry.source_ordinal,
                entry.context_index,
                raw_items.len()
            )));
        };
        let expected_item = entry.visible_response_item();
        if host_item != &expected_item {
            return Err(SpineError::CompactFailure(format!(
                "spine.close memory source entry mismatch at ordinal {} context_index {} source_hash {}",
                entry.source_ordinal, entry.context_index, entry.source_hash
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
struct SpineMemoryAssemblySkeleton {
    node_id: String,
    blocks: Vec<SpineMemoryAssemblyBlock>,
}

#[cfg(test)]
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

#[cfg(test)]
impl SpineMemoryAssemblySkeleton {
    fn from_source_plan(
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

    fn assemble(&self, node_memory: &str) -> Result<String, SpineError> {
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

#[cfg(test)]
fn push_memory_block(body: &mut String, heading: &str, block_body: &str) {
    body.push('\n');
    body.push_str(heading);
    body.push('\n');
    body.push_str(block_body.trim_matches('\n'));
    body.push('\n');
}

#[cfg(test)]
fn validate_generated_node_memory_body(body: &str) -> Result<(), SpineError> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Err(SpineError::CompactFailure(
            "spine.close memory argument produced empty node memory".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "close_memory_assembly_fixtures.rs"]
mod spine_close_memory_assembly_fixtures;
