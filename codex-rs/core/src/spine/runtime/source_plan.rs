use codex_protocol::models::ResponseItem;

use super::SpineCompactSourceEntryKind;
use super::SpineCompactSourcePlan;
use super::SpineError;
use super::SpineRuntime;
use super::support::collect_source_plan_entries_from_visible_refs;
use super::support::validate_source_plan_context_index;
use crate::spine::io::hash_response_items;
use crate::spine::model::ControlSymbol;
use crate::spine::model::NodeId;
use crate::spine::model::SpineTreeNode;
use crate::spine::model::Symbol;
use crate::spine::render::project_spine_tree_nodes_visible_items;

impl SpineRuntime {
    pub(crate) fn build_close_source_plan(
        &self,
        raw_context_items: &[ResponseItem],
        node: &NodeId,
        suffix_start: usize,
        toolcall_start: usize,
        close_call_id: &str,
    ) -> Result<SpineCompactSourcePlan, SpineError> {
        let open_meta = self.current_close_open_meta()?;
        if &open_meta.id != node {
            return Err(SpineError::Invariant(format!(
                "spine.close source plan requested for node {node}, but current close node is {}",
                open_meta.id
            )));
        }
        if open_meta.index != suffix_start {
            return Err(SpineError::CompactFailure(format!(
                "spine.close source plan suffix start {suffix_start} does not match h(PS) open index {} for node {node}",
                open_meta.index
            )));
        }
        if !self.parse_stack.current_open_has_nodes()? {
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

        let suffix_nodes = self.current_open_suffix_nodes()?;
        let visible_refs = project_spine_tree_nodes_visible_items(suffix_nodes, suffix_start)?;
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

        let mut previous_context_index = None;
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
                suffix_start,
                close_context_end,
                &mut previous_context_index,
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
        }

        let source_raw_start = self.open_raw_start(&open_meta.id)?;
        let source_raw_end =
            entries
                .iter()
                .try_fold(source_raw_start, |end, entry| -> Result<u64, SpineError> {
                    Ok(match &entry.kind {
                        SpineCompactSourceEntryKind::RawResponseItem { raw_ordinal, .. } => end
                            .max(raw_ordinal.checked_add(1).ok_or_else(|| {
                                SpineError::InvalidEvent(
                                    "spine.close source plan raw ordinal overflow".to_string(),
                                )
                            })?),
                        SpineCompactSourceEntryKind::ChildMemory {
                            source_raw_range, ..
                        } => end.max(source_raw_range.end),
                    })
                })?;

        Ok(SpineCompactSourcePlan {
            node_id: open_meta.id.clone(),
            source_context_range: suffix_start..close_context_end,
            source_raw_range: source_raw_start..source_raw_end,
            entries,
        })
    }

    fn current_open_suffix_nodes(&self) -> Result<&[SpineTreeNode], SpineError> {
        let open_idx = self
            .parse_stack
            .symbols
            .iter()
            .rposition(|symbol| matches!(symbol, Symbol::Control(ControlSymbol::Open(_))))
            .ok_or_else(|| SpineError::InvalidEvent("ParseStack has no live Open".to_string()))?;
        let suffix = &self.parse_stack.symbols[open_idx + 1..];
        match suffix {
            [Symbol::SpineTreeNodes(nodes)]
            | [
                Symbol::SpineTreeNodes(nodes),
                Symbol::Control(ControlSymbol::Close(_)),
            ] => Ok(nodes),
            _ => Err(SpineError::InvalidEvent(format!(
                "spine.close source plan expected live node list after current Open, found {suffix:?}"
            ))),
        }
    }
}
