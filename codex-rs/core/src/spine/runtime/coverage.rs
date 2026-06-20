use codex_protocol::models::ResponseItem;
use std::collections::BTreeSet;

use super::LiveRootCompact;
use super::SPINE_NAMESPACE;
use super::SPINE_TOOL_TREE;
use super::SpineError;
use super::SpineRuntime;
use super::support::ToolRawItemKind;
use super::support::is_spine_parser_control_tool_name;
use super::support::mark_raw_covered;
use super::support::mark_raw_prefix_covered;
use super::support::raw_item_requires_spine_coverage;
use super::support::tool_request_call_id;
use super::support::tool_response_call_id;
use crate::spine::model::RawMask;
use crate::spine::model::SpineLedgerEvent;

impl SpineRuntime {
    pub(crate) fn validate_raw_coverage(
        &self,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<(), SpineError> {
        if !self.jit_enabled {
            return Ok(());
        }
        let (
            spine_control_call_ids,
            spine_tree_call_ids,
            tool_request_call_ids,
            tool_response_call_ids,
        ) = raw_items
            .iter()
            .filter_map(|item| match item.as_ref()? {
                ResponseItem::FunctionCall {
                    call_id,
                    namespace: Some(namespace),
                    name,
                    ..
                } if namespace == SPINE_NAMESPACE && is_spine_parser_control_tool_name(name) => {
                    Some((call_id.clone(), ToolRawItemKind::SpineControlRequest))
                }
                ResponseItem::FunctionCall {
                    call_id,
                    namespace: Some(namespace),
                    name,
                    ..
                } if namespace == SPINE_NAMESPACE && name == SPINE_TOOL_TREE => {
                    Some((call_id.clone(), ToolRawItemKind::SpineTreeRequest))
                }
                item => tool_request_call_id(item)
                    .map(|call_id| (call_id.to_string(), ToolRawItemKind::Request))
                    .or_else(|| {
                        tool_response_call_id(item)
                            .map(|call_id| (call_id.to_string(), ToolRawItemKind::Response))
                    }),
            })
            .fold(
                (
                    BTreeSet::new(),
                    BTreeSet::new(),
                    BTreeSet::new(),
                    BTreeSet::new(),
                ),
                |(
                    mut spine_call_ids,
                    mut spine_tree_call_ids,
                    mut request_call_ids,
                    mut response_call_ids,
                ),
                 (call_id, kind)| {
                    match kind {
                        ToolRawItemKind::SpineControlRequest => {
                            spine_call_ids.insert(call_id.clone());
                            request_call_ids.insert(call_id);
                        }
                        ToolRawItemKind::SpineTreeRequest => {
                            spine_tree_call_ids.insert(call_id.clone());
                            request_call_ids.insert(call_id);
                        }
                        ToolRawItemKind::Request => {
                            request_call_ids.insert(call_id);
                        }
                        ToolRawItemKind::Response => {
                            response_call_ids.insert(call_id);
                        }
                    }
                    (
                        spine_call_ids,
                        spine_tree_call_ids,
                        request_call_ids,
                        response_call_ids,
                    )
                },
            );
        let completed_tool_call_ids = tool_request_call_ids
            .intersection(&tool_response_call_ids)
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut covered = vec![false; raw_items.len()];
        for event in &self.ledger.events {
            if !event.allowed_by(RawMask::new(&self.raw_live))? {
                continue;
            }
            match &event.event {
                SpineLedgerEvent::Msg { raw_ordinal, .. } => {
                    mark_raw_covered(&mut covered, *raw_ordinal)?;
                }
                SpineLedgerEvent::ToolCall { segments } => {
                    for segment in segments {
                        mark_raw_covered(&mut covered, segment.raw_ordinal)?;
                    }
                }
                SpineLedgerEvent::Open {
                    child,
                    boundary,
                    summary,
                    ..
                } => {
                    if !(summary == "root"
                        && child.parent().is_some_and(|parent| parent.is_root_epoch()))
                    {
                        mark_raw_covered(&mut covered, *boundary)?;
                    }
                }
                SpineLedgerEvent::Close { boundary, .. }
                | SpineLedgerEvent::RootCompact { boundary, .. } => {
                    mark_raw_prefix_covered(&mut covered, *boundary)?;
                }
                SpineLedgerEvent::Init { .. } | SpineLedgerEvent::OpenContextBaseline { .. } => {}
            }
        }
        for (index, item) in raw_items.iter().enumerate() {
            if item.as_ref().is_some_and(|item| {
                raw_item_requires_spine_coverage(
                    item,
                    &spine_control_call_ids,
                    &spine_tree_call_ids,
                    &completed_tool_call_ids,
                )
            }) && !covered[index]
            {
                return Err(SpineError::SidecarCorruption(format!(
                    "spine sidecar is missing token coverage for raw ordinal {index}; raw_len={} token_seq={}",
                    raw_items.len(),
                    self.ledger.next_event_seq
                )));
            }
        }
        Ok(())
    }

    pub(crate) fn live_root_compacts(&self) -> Result<Vec<LiveRootCompact>, SpineError> {
        if !self.jit_enabled {
            return Ok(Vec::new());
        }
        let raw_mask = RawMask::new(&self.raw_live);
        let mut compacts = Vec::new();
        for event in &self.ledger.events {
            if event.allowed_by(raw_mask)?
                && let SpineLedgerEvent::RootCompact { boundary, .. } = event.event
            {
                compacts.push(LiveRootCompact {
                    raw_boundary: boundary,
                    token_seq: event.seq,
                });
            }
        }
        Ok(compacts)
    }
}
