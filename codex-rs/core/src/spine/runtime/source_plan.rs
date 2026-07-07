use codex_protocol::models::ResponseItem;

use super::SpineCloseMemoryAssembly;
use super::SpineCompactSourceEntryKind;
use super::SpineCompactSourcePlan;
use super::SpineCompactSourcePlanEntry;
use super::SpineError;
use super::SpineRuntime;
use super::support::HostHistoryLens;
use crate::spine::io::hash_response_items;
use crate::spine::lexer::is_real_user_message;
use crate::spine::model::NodeId;
use crate::spine::render::VisibleItemRef;
use crate::spine::render::VisibleItemSource;
use crate::spine::render::memory_response_item;
use crate::spine::render::project_spine_tree_nodes_visible_items;
use crate::spine::render::read_memory_ref_body;
use crate::spine::render::render_memory_ref_context_items;
use crate::spine::render::visible_source_context_item_count;
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
        let projected_item_count = visible_refs.iter().try_fold(0usize, |total, visible_ref| {
            total
                .checked_add(visible_source_context_item_count(&visible_ref.source))
                .ok_or_else(|| {
                    SpineError::InvalidEvent(
                        "spine.close source plan visible item count overflow".to_string(),
                    )
                })
        })?;
        let projected_context_end =
            suffix_start
                .checked_add(projected_item_count)
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
        let mutable_context_len = HostHistoryLens::new(raw_context_items).mutable_len();
        if suffix_start >= mutable_context_len {
            return Err(SpineError::Operation(format!(
                "spine.close suffix start {suffix_start} is outside history length {} for node {node}",
                mutable_context_len
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
    let mut previous_context_end = None;
    let host_history = HostHistoryLens::new(raw_context_items);
    for (expected_ordinal, entry) in entries.iter().enumerate() {
        if entry.source_ordinal != expected_ordinal {
            return Err(SpineError::Invariant(format!(
                "spine.close source plan ordinal {} is not contiguous at expected ordinal {expected_ordinal}",
                entry.source_ordinal
            )));
        }
        validate_source_plan_context_index(
            entry.source_ordinal,
            entry.context_index,
            entry.context_item_count(),
            suffix_start,
            close_context_end,
            &mut previous_context_end,
        )?;
        let expected_items = entry.visible_response_items()?;
        let mut host_items = Vec::with_capacity(expected_items.len());
        for offset in 0..expected_items.len() {
            let context_index = entry.context_index.checked_add(offset).ok_or_else(|| {
                SpineError::InvalidEvent(
                    "spine.close source plan context index overflow".to_string(),
                )
            })?;
            host_items.push(
                host_history
                    .raw_item_for_mutable_index(context_index)
                    .map_err(|_| {
                        SpineError::CompactFailure(format!(
                            "spine.close source plan entry ordinal {} context_index {context_index} exceeds host history length {}",
                            entry.source_ordinal,
                            raw_context_items.len()
                        ))
                    })?
                    .clone(),
            );
        }
        let host_hash = hash_response_items(&host_items)?;
        if host_items != expected_items || host_hash != entry.source_hash {
            return Err(SpineError::CompactFailure(format!(
                "spine.close source plan mismatch at ordinal {} context_index {} source_hash {} host_hash {host_hash}",
                entry.source_ordinal, entry.context_index, entry.source_hash
            )));
        }
    }
    Ok(())
}

impl SpineCompactSourcePlanEntry {
    pub(crate) fn visible_response_items(&self) -> Result<Vec<ResponseItem>, SpineError> {
        match &self.kind {
            SpineCompactSourceEntryKind::RawResponseItem { item, .. } => Ok(vec![item.clone()]),
            SpineCompactSourceEntryKind::ChildMemory {
                rendered_context_item_count,
                body,
                ..
            } => render_child_memory_context_items(*rendered_context_item_count, body),
        }
    }

    pub(crate) fn context_item_count(&self) -> usize {
        match &self.kind {
            SpineCompactSourceEntryKind::RawResponseItem { .. } => 1,
            SpineCompactSourceEntryKind::ChildMemory {
                rendered_context_item_count,
                ..
            } => rendered_context_item_count.unwrap_or(1),
        }
    }
}

fn collect_source_plan_entries_from_visible_refs(
    visible_refs: &[VisibleItemRef],
    raw_context_items: &[ResponseItem],
) -> Result<Vec<SpineCompactSourcePlanEntry>, SpineError> {
    let mut entries = Vec::with_capacity(visible_refs.len());
    let host_history = HostHistoryLens::new(raw_context_items);
    for visible_ref in visible_refs {
        let (raw_ordinal, from_user, user_anchor) = match &visible_ref.source {
            VisibleItemSource::RawResponseItem {
                raw_ordinal,
                from_user,
                user_anchor,
            } => (*raw_ordinal, *from_user, *user_anchor),
            VisibleItemSource::ToolCallSegment { raw_ordinal, .. } => (*raw_ordinal, false, None),
            VisibleItemSource::MemoryRef { memory, .. } => {
                let source_ordinal = entries.len();
                let body = read_memory_ref_body(memory)?;
                let visible_items = render_memory_ref_context_items(memory, &body)?;
                let source_hash = hash_response_items(&visible_items)?;
                entries.push(SpineCompactSourcePlanEntry {
                    context_index: visible_ref.context_index,
                    source_ordinal,
                    source_hash,
                    kind: SpineCompactSourceEntryKind::ChildMemory {
                        node_id: memory.node_id.clone(),
                        compact_id: memory.compact_id.clone(),
                        source_raw_range: memory.source_raw_range.clone(),
                        rendered_context_item_count: memory.rendered_context_item_count,
                        body,
                        body_hash: memory.body_hash.clone(),
                    },
                });
                continue;
            }
            VisibleItemSource::MemorySeg { memory_id, .. } => {
                return Err(SpineError::CompactFailure(format!(
                    "spine.close source plan cannot trust SegRef::Memory {memory_id} without MemoryRef body_hash provenance"
                )));
            }
        };
        entries.push(source_plan_entry_from_response_item(
            entries.len(),
            raw_ordinal,
            visible_ref.context_index,
            from_user,
            user_anchor,
            &host_history,
        )?);
    }
    Ok(entries)
}

fn source_plan_entry_from_response_item(
    source_ordinal: usize,
    raw_ordinal: u64,
    context_index: usize,
    from_user: bool,
    user_anchor: Option<u64>,
    host_history: &HostHistoryLens<'_>,
) -> Result<SpineCompactSourcePlanEntry, SpineError> {
    let item = host_history
        .raw_item_for_mutable_index(context_index)?
        .clone();
    let source_hash = hash_response_items(std::slice::from_ref(&item))?;
    Ok(SpineCompactSourcePlanEntry {
        context_index,
        source_ordinal,
        source_hash,
        kind: SpineCompactSourceEntryKind::RawResponseItem {
            item,
            raw_ordinal,
            from_user,
            user_anchor,
        },
    })
}

fn validate_source_plan_context_index(
    source_ordinal: usize,
    context_index: usize,
    context_item_count: usize,
    suffix_start: usize,
    source_context_end: usize,
    previous_context_end: &mut Option<usize>,
) -> Result<(), SpineError> {
    if context_item_count == 0 {
        return Err(SpineError::CompactFailure(format!(
            "spine.close source plan entry ordinal {source_ordinal} covers zero context items"
        )));
    }
    if context_index < suffix_start {
        return Err(SpineError::CompactFailure(format!(
            "spine.close source plan entry ordinal {source_ordinal} context_index {context_index} precedes suffix start {suffix_start}"
        )));
    }
    let context_end = context_index
        .checked_add(context_item_count)
        .ok_or_else(|| {
            SpineError::InvalidEvent("spine.close source plan context range overflow".to_string())
        })?;
    if context_end > source_context_end {
        return Err(SpineError::CompactFailure(format!(
            "spine.close source plan entry ordinal {source_ordinal} context range [{context_index}..{context_end}) is outside source context range [{suffix_start}..{source_context_end})"
        )));
    }
    if let Some(previous_end) = *previous_context_end
        && context_index < previous_end
    {
        return Err(SpineError::CompactFailure(format!(
            "spine.close source plan entry ordinal {source_ordinal} context_index {context_index} overlaps previous context_end {previous_end}"
        )));
    }
    *previous_context_end = Some(context_end);
    Ok(())
}

fn render_child_memory_context_items(
    rendered_context_item_count: Option<usize>,
    body: &str,
) -> Result<Vec<ResponseItem>, SpineError> {
    let Some(expected_count) = rendered_context_item_count else {
        return Ok(vec![memory_response_item(body)]);
    };
    let items: Vec<ResponseItem> = serde_json::from_str(body).map_err(|err| {
        SpineError::InvalidStore(format!(
            "child memory body is not valid ResponseItem JSON: {err}"
        ))
    })?;
    if items.len() != expected_count {
        return Err(SpineError::InvalidStore(format!(
            "child memory rendered item count {expected_count} does not match body item count {}",
            items.len()
        )));
    }
    if items.is_empty() {
        return Err(SpineError::InvalidStore(
            "child memory has empty rendered context items".to_string(),
        ));
    }
    Ok(items)
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
    if node_memory.trim().is_empty() {
        return Err(SpineError::CompactFailure(
            "spine.close memory argument produced empty node memory".to_string(),
        ));
    }
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
