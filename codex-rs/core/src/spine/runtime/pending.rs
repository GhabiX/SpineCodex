use std::collections::BTreeSet;
#[cfg(test)]
use std::collections::btree_map::Entry;

use super::IntoSpineNodeMemory;
use super::SpineError;
use super::SpinePendingCloseAction;
use super::SpinePendingCommit;
use super::SpineRuntime;
use super::support::is_spine_parser_control_tool_name;
use super::support::user_anchor_refs_in_memory;
#[cfg(test)]
use super::support::validate_model_node_memory;
use crate::spine::lexer::ControlIntent;
use crate::spine::lexer::ParsedControlToolIntent;
use crate::spine::model::RawMask;
use crate::spine::model::SpineLedgerEvent;
use crate::spine::model::ToolCallSegmentKind;
use codex_protocol::models::ResponseItem;

#[derive(Clone, Debug)]
pub(super) struct OpenRequestAnchor {
    pub(super) raw_ordinal: u64,
    pub(super) context_index: u64,
}

#[derive(Clone, Debug)]
pub(super) struct PendingMemoryContextAccounting {
    pub(super) compact_id: String,
    pub(super) replacement_prefix_baseline_tokens: i64,
    pub(super) close_input_tokens: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ToolRequestAnchor {
    pub(crate) raw_ordinal: u64,
    pub(crate) context_index: usize,
}

#[derive(Clone, Debug)]
pub(super) enum PendingTransition {
    Open {
        call_id: String,
        summary: String,
        boundary: u64,
        index: u64,
    },
    Close {
        call_id: String,
        memory: String,
    },
    NextSugar {
        call_id: String,
        summary: String,
        memory: String,
    },
}

impl PendingTransition {
    pub(super) fn call_id(&self) -> &str {
        match self {
            Self::Open { call_id, .. }
            | Self::Close { call_id, .. }
            | Self::NextSugar { call_id, .. } => call_id,
        }
    }

    pub(super) fn control_intent(&self) -> ControlIntent {
        match self {
            Self::Open { .. } => ControlIntent::Open,
            Self::Close { .. } => ControlIntent::Close,
            Self::NextSugar { .. } => ControlIntent::Next,
        }
    }
}

#[cfg(test)]
#[derive(Clone, Debug)]
pub(super) enum SpineControlToolReceipt {
    Open { summary: String },
    Close { memory: String },
    Next { summary: String, memory: String },
}

#[cfg(test)]
impl SpineControlToolReceipt {
    pub(super) fn is_close_like(&self) -> bool {
        matches!(self, Self::Close { .. } | Self::Next { .. })
    }
}

#[derive(Clone, Debug)]
pub(super) struct PendingMsg {
    pub(super) raw_ordinal: u64,
    pub(super) context_index: u64,
    pub(super) from_user: bool,
    pub(super) user_anchor: Option<u64>,
}

#[derive(Clone, Debug)]
pub(super) struct PendingToolRequest {
    pub(super) raw_ordinal: u64,
    pub(super) context_index: u64,
}

#[cfg(test)]
#[derive(Clone, Debug)]
pub(super) struct PendingToolResponse {
    pub(super) raw_ordinal: u64,
    pub(super) context_index: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct CompletedToolCall {
    pub(crate) call_id: String,
    pub(crate) request_call_ids: Vec<String>,
    pub(crate) segments: Vec<CompletedToolCallSegment>,
}

#[derive(Clone, Debug)]
pub(crate) struct CompletedToolCallSegment {
    pub(crate) kind: ToolCallSegmentKind,
    pub(crate) raw_ordinal: u64,
    pub(crate) context_index: usize,
}

impl SpineRuntime {
    #[cfg(test)]
    pub(super) fn validate_control_tool_receipt_pending_view(
        &self,
        receipt: &SpineControlToolReceipt,
    ) -> Result<(), SpineError> {
        match receipt {
            SpineControlToolReceipt::Open { summary } => {
                if summary.trim().is_empty() {
                    return Err(SpineError::ToolUse(
                        "spine.open summary must not be empty".to_string(),
                    ));
                }
            }
            SpineControlToolReceipt::Close { memory } => {
                validate_model_node_memory(memory)?;
                self.validate_memory_user_anchor_refs(memory)?;
            }
            SpineControlToolReceipt::Next { summary, memory } => {
                if summary.trim().is_empty() {
                    return Err(SpineError::ToolUse(
                        "spine.next summary must not be empty".to_string(),
                    ));
                }
                validate_model_node_memory(memory)?;
                self.validate_memory_user_anchor_refs(memory)?;
            }
        }
        Ok(())
    }

    pub(crate) fn stage_open(
        &mut self,
        call_id: String,
        summary: String,
    ) -> Result<(), SpineError> {
        self.ensure_no_pending_transition()?;
        let summary = summary.trim().to_string();
        if summary.is_empty() {
            return Err(SpineError::ToolUse(
                "spine.open summary must not be empty".to_string(),
            ));
        }
        let anchor = self.open_requests.remove(&call_id).ok_or_else(|| {
            SpineError::Operation(format!(
                "missing spine.open request anchor for call_id={call_id}"
            ))
        })?;
        self.stage(PendingTransition::Open {
            call_id,
            summary,
            boundary: anchor.raw_ordinal,
            index: anchor.context_index,
        })
    }

    pub(crate) fn stage_close<M: IntoSpineNodeMemory>(
        &mut self,
        call_id: String,
        memory: M,
    ) -> Result<(), SpineError> {
        self.ensure_no_pending_transition()?;
        let memory = memory.into_spine_node_memory()?;
        self.validate_memory_user_anchor_refs(&memory)?;
        if !self.control_call_ids.contains(&call_id) {
            return Err(SpineError::Operation(format!(
                "missing spine.close request anchor for call_id={call_id}"
            )));
        }
        self.current_close_open_meta()?;
        self.stage(PendingTransition::Close { call_id, memory })
    }

    pub(crate) fn stage_next<M: IntoSpineNodeMemory>(
        &mut self,
        call_id: String,
        summary: String,
        memory: M,
    ) -> Result<(), SpineError> {
        self.ensure_no_pending_transition()?;
        let summary = summary.trim().to_string();
        if summary.is_empty() {
            return Err(SpineError::ToolUse(
                "spine.next summary must not be empty".to_string(),
            ));
        }
        let memory = memory.into_spine_node_memory()?;
        self.validate_memory_user_anchor_refs(&memory)?;
        if !self.control_call_ids.contains(&call_id) {
            return Err(SpineError::Operation(format!(
                "missing spine.next request anchor for call_id={call_id}"
            )));
        }
        self.current_close_open_meta()?;
        self.stage(PendingTransition::NextSugar {
            call_id,
            summary,
            memory,
        })
    }

    fn validate_memory_user_anchor_refs(&self, memory: &str) -> Result<(), SpineError> {
        let refs = user_anchor_refs_in_memory(memory)?;
        if refs.is_empty() {
            return Ok(());
        }
        let existing = self.live_user_anchors()?;
        for anchor in refs {
            if !existing.contains(&anchor) {
                return Err(SpineError::ToolUse(format!(
                    "spine.close/next memory references unknown user anchor [U{anchor}]"
                )));
            }
        }
        Ok(())
    }

    fn live_user_anchors(&self) -> Result<BTreeSet<u64>, SpineError> {
        let raw_mask = RawMask::new(&self.raw_live);
        let mut anchors = BTreeSet::new();
        for event in &self.ledger.events {
            if !event.allowed_by(raw_mask)? {
                continue;
            }
            if let SpineLedgerEvent::Msg {
                user_anchor: Some(anchor),
                ..
            } = &event.event
            {
                anchors.insert(*anchor);
            }
        }
        Ok(anchors)
    }

    fn stage(&mut self, pending: PendingTransition) -> Result<(), SpineError> {
        self.ensure_no_pending_transition()?;
        self.pending = Some(pending);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn record_open_tool_receipt(
        &mut self,
        call_id: String,
        summary: String,
    ) -> Result<(), SpineError> {
        self.record_control_tool_receipt(call_id, SpineControlToolReceipt::Open { summary })
    }

    #[cfg(test)]
    pub(crate) fn record_close_tool_receipt(
        &mut self,
        call_id: String,
        memory: String,
    ) -> Result<(), SpineError> {
        self.record_control_tool_receipt(call_id, SpineControlToolReceipt::Close { memory })
    }

    #[cfg(test)]
    pub(crate) fn record_next_tool_receipt(
        &mut self,
        call_id: String,
        summary: String,
        memory: String,
    ) -> Result<(), SpineError> {
        self.record_control_tool_receipt(call_id, SpineControlToolReceipt::Next { summary, memory })
    }

    #[cfg(test)]
    fn record_control_tool_receipt(
        &mut self,
        call_id: String,
        receipt: SpineControlToolReceipt,
    ) -> Result<(), SpineError> {
        self.ensure_jit_enabled("Spine control tool receipt")?;
        if !self.control_call_ids.contains(&call_id) {
            return Err(SpineError::Operation(format!(
                "missing Spine control request anchor for call_id={call_id}"
            )));
        }
        match self.control_receipts.entry(call_id.clone()) {
            Entry::Vacant(entry) => {
                entry.insert(receipt);
            }
            Entry::Occupied(_) => {
                return Err(SpineError::InvalidEvent(format!(
                    "duplicate Spine control receipt for call_id={call_id}"
                )));
            }
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn ensure_pending_from_receipt(&mut self, call_id: &str) -> Result<(), SpineError> {
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.call_id() == call_id)
        {
            return Ok(());
        }
        let Some(receipt) = self.control_receipts.get(call_id).cloned() else {
            return Ok(());
        };
        match receipt {
            SpineControlToolReceipt::Open { summary } => {
                self.stage_open(call_id.to_string(), summary)?;
            }
            SpineControlToolReceipt::Close { memory } => {
                self.stage_close(call_id.to_string(), memory)?;
            }
            SpineControlToolReceipt::Next { summary, memory } => {
                self.stage_next(call_id.to_string(), summary, memory)?;
            }
        };
        self.control_receipts.remove(call_id);
        Ok(())
    }

    pub(crate) fn ensure_pending_from_toolcall_request(
        &mut self,
        call_id: &str,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<(), SpineError> {
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.call_id() == call_id)
        {
            return Ok(());
        }
        if !self.control_call_ids.contains(call_id) {
            return Ok(());
        }
        let request = self
            .ordinary_tool_requests
            .get(call_id)
            .ok_or_else(|| {
                SpineError::Operation(format!(
                    "missing Spine control request anchor for call_id={call_id}"
                ))
            })?
            .clone();
        let raw_index = usize::try_from(request.raw_ordinal).map_err(|_| {
            SpineError::InvalidEvent("Spine control request raw ordinal overflow".to_string())
        })?;
        let item = raw_items
            .get(raw_index)
            .and_then(Option::as_ref)
            .ok_or_else(|| {
                SpineError::InvalidEvent(format!(
                    "missing Spine control request raw item for call_id={call_id}"
                ))
            })?;
        let ResponseItem::FunctionCall {
            call_id: request_call_id,
            name,
            namespace: Some(namespace),
            arguments,
            ..
        } = item
        else {
            return Err(SpineError::InvalidEvent(format!(
                "Spine control request raw item for call_id={call_id} is not a function call"
            )));
        };
        if request_call_id != call_id {
            return Err(SpineError::InvalidEvent(format!(
                "Spine control request raw item call_id={request_call_id} does not match completed call_id={call_id}"
            )));
        }
        if namespace != super::SPINE_NAMESPACE || !is_spine_parser_control_tool_name(name) {
            return Err(SpineError::InvalidEvent(format!(
                "raw item for call_id={call_id} is not a Spine parser control request"
            )));
        }
        match crate::spine::lexer::parse_control_tool_intent(name, arguments)? {
            Some(ParsedControlToolIntent::Open { summary }) => {
                self.stage_open(call_id.to_string(), summary)
            }
            Some(ParsedControlToolIntent::Close { memory }) => {
                self.stage_close(call_id.to_string(), memory)
            }
            Some(ParsedControlToolIntent::Next { summary, memory }) => {
                self.stage_next(call_id.to_string(), summary, memory)
            }
            None => Ok(()),
        }
    }

    pub(crate) fn has_close_like_control_request(
        &self,
        call_id: &str,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<bool, SpineError> {
        if self.pending.as_ref().is_some_and(|pending| {
            pending.call_id() == call_id && pending.control_intent().is_close_like()
        }) {
            return Ok(true);
        }
        #[cfg(test)]
        if self
            .control_receipts
            .get(call_id)
            .is_some_and(SpineControlToolReceipt::is_close_like)
        {
            return Ok(true);
        }
        if !self.control_call_ids.contains(call_id) {
            return Ok(false);
        }
        let Some(request) = self.ordinary_tool_requests.get(call_id) else {
            return Ok(false);
        };
        let raw_index = usize::try_from(request.raw_ordinal).map_err(|_| {
            SpineError::InvalidEvent("Spine control request raw ordinal overflow".to_string())
        })?;
        Ok(matches!(
            raw_items.get(raw_index).and_then(Option::as_ref),
            Some(ResponseItem::FunctionCall {
                call_id: request_call_id,
                name,
                namespace: Some(namespace),
                ..
            }) if request_call_id == call_id
                && namespace == super::SPINE_NAMESPACE
                && matches!(name.as_str(), super::SPINE_TOOL_CLOSE | super::SPINE_TOOL_NEXT)
        ))
    }

    fn ensure_no_pending_transition(&self) -> Result<(), SpineError> {
        if self.pending.is_some() {
            let pending_call_id = self
                .pending
                .as_ref()
                .map(PendingTransition::call_id)
                .unwrap_or("<unknown>");
            return Err(SpineError::Operation(format!(
                "another spine transition is already pending: call_id={pending_call_id}"
            )));
        }
        Ok(())
    }

    pub(crate) fn abort_pending(&mut self, call_id: &str) -> bool {
        #[cfg(test)]
        let removed_receipt = self.control_receipts.remove(call_id).is_some();
        #[cfg(not(test))]
        let removed_receipt = false;
        if self
            .pending
            .as_ref()
            .is_none_or(|pending| pending.call_id() != call_id)
        {
            if removed_receipt {
                self.control_call_ids.remove(call_id);
            }
            return removed_receipt;
        }
        let Some(pending) = self.pending.take() else {
            if removed_receipt {
                self.control_call_ids.remove(call_id);
            }
            return removed_receipt;
        };
        self.control_call_ids.remove(pending.call_id());
        true
    }

    pub(crate) fn abort_any_pending(&mut self) -> Option<String> {
        let pending = self.pending.take()?;
        let call_id = pending.call_id().to_string();
        self.control_call_ids.remove(&call_id);
        #[cfg(test)]
        self.control_receipts.remove(&call_id);
        Some(call_id)
    }

    pub(crate) fn pending_commit(
        &self,
        call_id: &str,
    ) -> Result<Option<SpinePendingCommit>, SpineError> {
        if let Some(pending) = self.pending.as_ref()
            && pending.call_id() == call_id
        {
            return Ok(Some(match pending {
                PendingTransition::Open { .. } => SpinePendingCommit::Open,
                PendingTransition::Close { memory, .. } => {
                    let open_meta = self.current_close_open_meta()?;
                    SpinePendingCommit::Close {
                        action: SpinePendingCloseAction::Close,
                        node: open_meta.id.clone(),
                        suffix_start: open_meta.index,
                        memory: memory.clone(),
                        next_summary: None,
                    }
                }
                PendingTransition::NextSugar {
                    summary, memory, ..
                } => {
                    let open_meta = self.current_close_open_meta()?;
                    SpinePendingCommit::Close {
                        action: SpinePendingCloseAction::Next,
                        node: open_meta.id.clone(),
                        suffix_start: open_meta.index,
                        memory: memory.clone(),
                        next_summary: Some(summary.clone()),
                    }
                }
            }));
        }
        #[cfg(test)]
        {
            return Ok(self
                .control_receipts
                .get(call_id)
                .map(|receipt| {
                    self.validate_control_tool_receipt_pending_view(receipt)?;
                    match receipt {
                        SpineControlToolReceipt::Open { .. } => {
                            Ok::<SpinePendingCommit, SpineError>(SpinePendingCommit::Open)
                        }
                        SpineControlToolReceipt::Close { memory } => {
                            let open_meta = self.current_close_open_meta()?;
                            Ok(SpinePendingCommit::Close {
                                action: SpinePendingCloseAction::Close,
                                node: open_meta.id.clone(),
                                suffix_start: open_meta.index,
                                memory: memory.clone(),
                                next_summary: None,
                            })
                        }
                        SpineControlToolReceipt::Next { summary, memory } => {
                            let open_meta = self.current_close_open_meta()?;
                            Ok(SpinePendingCommit::Close {
                                action: SpinePendingCloseAction::Next,
                                node: open_meta.id.clone(),
                                suffix_start: open_meta.index,
                                memory: memory.clone(),
                                next_summary: Some(summary.clone()),
                            })
                        }
                    }
                })
                .transpose()?);
        }
        #[cfg(not(test))]
        Ok(None)
    }

    pub(crate) fn has_close_like_pending_commit(&self, call_id: &str) -> Result<bool, SpineError> {
        Ok(matches!(
            self.pending_commit(call_id)?,
            Some(SpinePendingCommit::Close { .. })
        ))
    }

    #[cfg(test)]
    pub(crate) fn has_close_like_control_receipt(&self, call_id: &str) -> bool {
        self.control_receipts
            .get(call_id)
            .is_some_and(SpineControlToolReceipt::is_close_like)
            || self.pending.as_ref().is_some_and(|pending| {
                pending.call_id() == call_id && pending.control_intent().is_close_like()
            })
    }

    pub(crate) fn pending_tool_request_anchor(
        &self,
        call_id: &str,
    ) -> Result<ToolRequestAnchor, SpineError> {
        if let Some(anchor) = self.open_requests.get(call_id) {
            return Ok(ToolRequestAnchor {
                raw_ordinal: anchor.raw_ordinal,
                context_index: usize::try_from(anchor.context_index).map_err(|_| {
                    SpineError::InvalidEvent("spine.open context index overflow".to_string())
                })?,
            });
        }
        let Some(request) = self.ordinary_tool_requests.get(call_id) else {
            return Err(SpineError::Operation(format!(
                "missing tool request anchor for call_id={call_id}"
            )));
        };
        Ok(ToolRequestAnchor {
            raw_ordinal: request.raw_ordinal,
            context_index: usize::try_from(request.context_index).map_err(|_| {
                SpineError::InvalidEvent("tool request context index overflow".to_string())
            })?,
        })
    }

    pub(crate) fn is_control_output_call_id(&self, call_id: &str) -> bool {
        self.control_call_ids.contains(call_id)
            || self
                .pending
                .as_ref()
                .is_some_and(|pending| pending.call_id() == call_id)
    }

    pub(crate) fn has_pending_tool_request(&self) -> bool {
        !self.ordinary_tool_requests.is_empty()
    }
}
