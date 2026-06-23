use codex_protocol::models::ResponseItem;

use super::SpineCloseMemoryAssembly;
use super::SpineCompactSourceEntryKind;
use super::SpineCompactSourcePlan;
use super::SpineCompactSourcePlanEntry;
use super::SpineError;
use super::SpineRuntime;
use super::support::collect_source_plan_entries_from_visible_refs;
use super::support::is_real_user_message;
use super::support::validate_source_plan_context_index;
use crate::spine::io::hash_response_items;
use crate::spine::model::NodeId;
use crate::spine::render::project_spine_tree_nodes_visible_items;
use crate::spine::user_message_projection::user_message_memory_body;

impl SpineRuntime {
    pub(crate) fn prepare_close_memory_assembly_for_completed_toolcall(
        &self,
        raw_context_items: &[ResponseItem],
        toolcall_start: usize,
        call_id: &str,
    ) -> Result<Option<SpineCloseMemoryAssembly>, SpineError> {
        let Some(pending_commit) = self.pending_commit(call_id)? else {
            return Ok(None);
        };
        let super::SpinePendingCommit::Close {
            node,
            suffix_start,
            memory,
            action: _,
            next_summary: _,
        } = pending_commit
        else {
            return Ok(None);
        };
        let source_plan = self.build_close_source_plan(
            raw_context_items,
            &node,
            suffix_start,
            toolcall_start,
            call_id,
        )?;
        close_memory_assembly_from_source_plan(&node, &source_plan, &memory).map(Some)
    }

    pub(crate) fn build_close_source_plan(
        &self,
        raw_context_items: &[ResponseItem],
        node: &NodeId,
        suffix_start: usize,
        toolcall_start: usize,
        close_call_id: &str,
    ) -> Result<SpineCompactSourcePlan, SpineError> {
        let open_meta = self.current_close_open_meta()?;
        let close_context_end = self.validate_close_source_plan_request(
            raw_context_items,
            node,
            suffix_start,
            toolcall_start,
            close_call_id,
            open_meta.index,
            &open_meta.id,
        )?;

        let suffix_nodes = self.parser.current_open_suffix_nodes_cloned()?;
        let visible_refs = project_spine_tree_nodes_visible_items(&suffix_nodes, suffix_start)?;
        let projected_context_end =
            suffix_start
                .checked_add(visible_refs.len())
                .ok_or_else(|| {
                    SpineError::InvalidEvent(
                        "spine.close source plan context range overflow".to_string(),
                    )
                })?;
        if projected_context_end != close_context_end {
            return Err(SpineError::CompactFailure(format!(
                "spine.close h(PS) suffix projects to [{suffix_start}..{projected_context_end}) but source context range is [{suffix_start}..{close_context_end}) for node {node} call_id={close_call_id}"
            )));
        }
        let entries =
            collect_source_plan_entries_from_visible_refs(&visible_refs, raw_context_items)?;

        if entries.is_empty() {
            return Err(SpineError::Operation(format!(
                "spine.close requires non-empty live suffix for node {}",
                open_meta.id
            )));
        }

        validate_close_source_plan_entries(
            &entries,
            raw_context_items,
            suffix_start,
            close_context_end,
        )?;

        let source_raw_start = self.open_raw_start(&open_meta.id)?;
        let source_raw_end = close_source_plan_raw_end(source_raw_start, &entries)?;

        Ok(SpineCompactSourcePlan {
            node_id: open_meta.id.clone(),
            source_context_range: suffix_start..close_context_end,
            source_raw_range: source_raw_start..source_raw_end,
            entries,
        })
    }

    fn validate_close_source_plan_request(
        &self,
        raw_context_items: &[ResponseItem],
        node: &NodeId,
        suffix_start: usize,
        toolcall_start: usize,
        close_call_id: &str,
        open_index: usize,
        open_node: &NodeId,
    ) -> Result<usize, SpineError> {
        if open_node != node {
            return Err(SpineError::Invariant(format!(
                "spine.close source plan requested for node {node}, but current close node is {}",
                open_node
            )));
        }
        if open_index != suffix_start {
            return Err(SpineError::CompactFailure(format!(
                "spine.close source plan suffix start {suffix_start} does not match h(PS) open index {open_index} for node {node}",
            )));
        }
        if !self.parser.current_open_has_nodes()? {
            return Err(SpineError::Operation(format!(
                "spine.close requires non-empty live suffix for node {node}"
            )));
        }
        if suffix_start >= raw_context_items.len() {
            return Err(SpineError::Operation(format!(
                "spine.close suffix start {suffix_start} is outside history length {} for node {node}",
                raw_context_items.len()
            )));
        }

        let close_context_end = toolcall_start;
        if close_context_end < suffix_start {
            return Err(SpineError::Operation(format!(
                "spine.close request index {close_context_end} precedes suffix start {suffix_start} for node {node} call_id={close_call_id}"
            )));
        }
        if close_context_end == suffix_start {
            return Err(SpineError::Operation(format!(
                "spine.close requires non-empty live suffix for node {node} call_id={close_call_id}"
            )));
        }
        Ok(close_context_end)
    }
}

fn validate_close_source_plan_entries(
    entries: &[SpineCompactSourcePlanEntry],
    raw_context_items: &[ResponseItem],
    suffix_start: usize,
    close_context_end: usize,
) -> Result<(), SpineError> {
    let mut previous_context_index = None;
    for (expected_ordinal, entry) in entries.iter().enumerate() {
        validate_close_source_plan_entry(
            entry,
            expected_ordinal,
            raw_context_items,
            suffix_start,
            close_context_end,
            &mut previous_context_index,
        )?;
    }
    Ok(())
}

fn validate_close_source_plan_entry(
    entry: &SpineCompactSourcePlanEntry,
    expected_ordinal: usize,
    raw_context_items: &[ResponseItem],
    suffix_start: usize,
    close_context_end: usize,
    previous_context_index: &mut Option<usize>,
) -> Result<(), SpineError> {
    if entry.source_ordinal != expected_ordinal {
        return Err(SpineError::Invariant(format!(
            "spine.close source plan ordinal {} is not contiguous at expected ordinal {expected_ordinal}",
            entry.source_ordinal
        )));
    }
    validate_source_plan_context_index(
        entry.source_ordinal,
        entry.context_index,
        suffix_start,
        close_context_end,
        previous_context_index,
    )?;
    let host_item = raw_context_items.get(entry.context_index).ok_or_else(|| {
        SpineError::CompactFailure(format!(
            "spine.close source plan entry ordinal {} context_index {} exceeds host history length {}",
            entry.source_ordinal,
            entry.context_index,
            raw_context_items.len()
        ))
    })?;
    let expected_item = entry.visible_response_item();
    let host_hash = hash_response_items(std::slice::from_ref(host_item))?;
    if host_item != &expected_item || host_hash != entry.source_hash {
        return Err(SpineError::CompactFailure(format!(
            "spine.close source plan mismatch at ordinal {} context_index {} source_hash {} host_hash {host_hash}",
            entry.source_ordinal, entry.context_index, entry.source_hash
        )));
    }
    Ok(())
}

fn close_source_plan_raw_end(
    source_raw_start: u64,
    entries: &[SpineCompactSourcePlanEntry],
) -> Result<u64, SpineError> {
    entries
        .iter()
        .try_fold(source_raw_start, |end, entry| -> Result<u64, SpineError> {
            Ok(match &entry.kind {
                SpineCompactSourceEntryKind::RawResponseItem { raw_ordinal, .. } => {
                    end.max(raw_ordinal.checked_add(1).ok_or_else(|| {
                        SpineError::InvalidEvent(
                            "spine.close source plan raw ordinal overflow".to_string(),
                        )
                    })?)
                }
                SpineCompactSourceEntryKind::ChildMemory {
                    source_raw_range, ..
                } => end.max(source_raw_range.end),
            })
        })
}

fn close_memory_assembly_from_source_plan(
    node_id: &NodeId,
    source_plan: &SpineCompactSourcePlan,
    node_memory: &str,
) -> Result<SpineCloseMemoryAssembly, SpineError> {
    if &source_plan.node_id != node_id {
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
    let body = assemble_close_memory_body(node_id, source_plan, node_memory)?;
    Ok(SpineCloseMemoryAssembly {
        body,
        source_context_range: source_plan.source_context_range.clone(),
        source_raw_range: source_plan.source_raw_range.clone(),
        memory_output_tokens: None,
    })
}

fn assemble_close_memory_body(
    node_id: &NodeId,
    source_plan: &SpineCompactSourcePlan,
    node_memory: &str,
) -> Result<String, SpineError> {
    let mut body = format!("# Spine Memory {node_id}\n");
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
                    let heading = user_anchor
                        .map(|anchor| format!("## User Message [U{anchor}]"))
                        .unwrap_or_else(|| "## User Message".to_string());
                    push_memory_block(&mut body, &heading, &text);
                }
            }
            SpineCompactSourceEntryKind::RawResponseItem {
                from_user: false, ..
            } => {}
            SpineCompactSourceEntryKind::ChildMemory { body: child, .. } => {
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
