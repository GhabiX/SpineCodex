use codex_protocol::models::ResponseItem;
use std::collections::BTreeSet;

use super::LiveRootCompact;
use super::SpineError;
use super::SpineRuntime;
use super::support::mark_raw_covered;
use super::support::mark_raw_prefix_covered;
use super::support::raw_item_requires_spine_coverage;
use super::support::tool_request_call_id;
use super::support::tool_response_call_id;
use crate::spine::model::LoggedSpineLedgerEvent;
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
        let completed_tool_call_ids = completed_raw_tool_call_ids(raw_items);
        let mut covered = vec![false; raw_items.len()];
        let raw_mask = RawMask::new(&self.raw_live);
        for event in &self.ledger.events {
            if !event.allowed_by(raw_mask)? {
                continue;
            }
            mark_raw_covered_by_event(&mut covered, event)?;
        }
        for (index, item) in raw_items.iter().enumerate() {
            if item.as_ref().is_some_and(|item| {
                raw_item_requires_spine_coverage(item, &completed_tool_call_ids)
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

fn mark_raw_covered_by_event(
    covered: &mut [bool],
    event: &LoggedSpineLedgerEvent,
) -> Result<(), SpineError> {
    match &event.event {
        SpineLedgerEvent::Msg { raw_ordinal, .. } => {
            mark_raw_covered(covered, *raw_ordinal)?;
        }
        SpineLedgerEvent::ToolCall { segments } => {
            for segment in segments {
                mark_raw_covered(covered, segment.raw_ordinal)?;
            }
        }
        SpineLedgerEvent::Open {
            child,
            boundary,
            summary,
            ..
        } => {
            if !(summary == "root" && child.parent().is_some_and(|parent| parent.is_root_epoch())) {
                mark_raw_covered(covered, *boundary)?;
            }
        }
        SpineLedgerEvent::Close { boundary, .. }
        | SpineLedgerEvent::RootCompact { boundary, .. } => {
            mark_raw_prefix_covered(covered, *boundary)?;
        }
        SpineLedgerEvent::Init { .. } | SpineLedgerEvent::OpenContextBaseline { .. } => {}
    }
    Ok(())
}

fn completed_raw_tool_call_ids(raw_items: &[Option<ResponseItem>]) -> BTreeSet<String> {
    let mut request = BTreeSet::new();
    let mut response = BTreeSet::new();
    for item in raw_items.iter().filter_map(Option::as_ref) {
        if let Some(call_id) = tool_request_call_id(item) {
            request.insert(call_id.to_string());
        } else if let Some(call_id) = tool_response_call_id(item) {
            response.insert(call_id.to_string());
        }
    }
    request.intersection(&response).cloned().collect()
}
