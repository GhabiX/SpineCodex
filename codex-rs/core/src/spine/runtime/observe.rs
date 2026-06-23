use codex_protocol::models::ResponseItem;

use super::CompletedToolCall;
use super::CompletedToolCallSegment;
use super::OpenRequestAnchor;
use super::PendingToolRequest;
#[cfg(test)]
use super::PendingToolResponse;
use super::SPINE_NAMESPACE;
use super::SPINE_TOOL_OPEN;
use super::SPINE_TOOL_TREE;
use super::SpineError;
use super::SpineRuntime;
use super::support::is_spine_parser_control_tool_name;
use super::support::tool_request_call_id;
use super::support::tool_response_call_id;
use crate::spine::lexer::ControlIntent;
use crate::spine::lexer::LexedTokenKind;
use crate::spine::lexer::lex_observed_msg;
use crate::spine::lexer::lex_toolcall_event_token;
use crate::spine::lexer::plan_control_toolcall;
use crate::spine::model::SpineLedgerEvent;
use crate::spine::model::SpineToken;
use crate::spine::model::ToolCallSegmentKind;

impl SpineRuntime {
    pub(crate) fn observe_raw_items(&mut self, count: usize) -> Result<(), SpineError> {
        let raw_count = u64::try_from(count)
            .map_err(|_| SpineError::InvalidEvent("raw item count overflow".to_string()))?;
        self.raw_len = self
            .raw_len
            .checked_add(raw_count)
            .ok_or_else(|| SpineError::InvalidEvent("raw ordinal overflow".to_string()))?;
        self.raw_live.extend(std::iter::repeat_n(true, count));
        Ok(())
    }

    pub(crate) fn observe_context_item(
        &mut self,
        raw_ordinal: u64,
        context_index: usize,
        item: &ResponseItem,
    ) -> Result<(), SpineError> {
        if !self.jit_enabled {
            return Ok(());
        }
        if tool_request_call_id(item).is_some() {
            return self.observe_toolcall_request_anchor(raw_ordinal, context_index, item);
        }
        if tool_response_call_id(item).is_some() {
            return self.observe_toolcall_response_anchor(raw_ordinal, context_index, item);
        }
        if matches!(
            item,
            ResponseItem::ToolSearchOutput { call_id: None, .. }
                | ResponseItem::ToolSearchCall { call_id: None, .. }
        ) {
            return Ok(());
        }
        self.on_non_toolcall_msg(raw_ordinal, context_index, item)
    }

    pub(crate) fn observe_toolcall_request_anchor(
        &mut self,
        raw_ordinal: u64,
        context_index: usize,
        item: &ResponseItem,
    ) -> Result<(), SpineError> {
        if !self.jit_enabled {
            return Ok(());
        }
        let context_index = u64::try_from(context_index)
            .map_err(|_| SpineError::InvalidEvent("context index overflow".to_string()))?;
        if let ResponseItem::FunctionCall {
            call_id,
            name,
            namespace: Some(namespace),
            ..
        } = item
            && namespace == SPINE_NAMESPACE
            && is_spine_parser_control_tool_name(name)
        {
            self.control_call_ids.insert(call_id.clone());
            if name == SPINE_TOOL_OPEN && self.open_requests.contains_key(call_id) {
                return Err(SpineError::InvalidEvent(format!(
                    "duplicate spine.open request anchor for {call_id}"
                )));
            }
            self.insert_tool_request_anchor(call_id, raw_ordinal, context_index, "tool request")?;
            if name == SPINE_TOOL_OPEN {
                self.open_requests.insert(
                    call_id.clone(),
                    OpenRequestAnchor {
                        raw_ordinal,
                        context_index,
                    },
                );
            }
            return Ok(());
        }
        if let ResponseItem::FunctionCall {
            call_id,
            name,
            namespace: Some(namespace),
            ..
        } = item
            && namespace == SPINE_NAMESPACE
            && name == SPINE_TOOL_TREE
        {
            self.tree_call_ids.insert(call_id.clone());
            return self.insert_tool_request_anchor(
                call_id,
                raw_ordinal,
                context_index,
                "tool request",
            );
        }
        let Some(call_id) = tool_request_call_id(item) else {
            return Err(SpineError::InvalidEvent(
                "observe_toolcall_request_anchor received non-request item".to_string(),
            ));
        };
        self.insert_tool_request_anchor(
            call_id,
            raw_ordinal,
            context_index,
            "ordinary tool request",
        )
    }

    pub(crate) fn observe_toolcall_response_anchor(
        &mut self,
        _raw_ordinal: u64,
        _context_index: usize,
        item: &ResponseItem,
    ) -> Result<(), SpineError> {
        if !self.jit_enabled {
            return Ok(());
        }
        let Some(call_id) = tool_response_call_id(item) else {
            return Err(SpineError::InvalidEvent(
                "observe_toolcall_response_anchor received non-response item".to_string(),
            ));
        };
        #[cfg(test)]
        let context_index = u64::try_from(_context_index)
            .map_err(|_| SpineError::InvalidEvent("context index overflow".to_string()))?;
        #[cfg(test)]
        self.pending_tool_responses
            .entry(call_id.to_string())
            .or_default()
            .push(PendingToolResponse {
                raw_ordinal: _raw_ordinal,
                context_index,
            });
        self.tree_call_ids.remove(call_id);
        Ok(())
    }

    fn insert_tool_request_anchor(
        &mut self,
        call_id: &str,
        raw_ordinal: u64,
        context_index: u64,
        duplicate_label: &str,
    ) -> Result<(), SpineError> {
        if self.ordinary_tool_requests.contains_key(call_id) {
            return Err(SpineError::InvalidEvent(format!(
                "duplicate {duplicate_label} anchor for {call_id}"
            )));
        }
        self.ordinary_tool_requests.insert(
            call_id.to_string(),
            PendingToolRequest {
                raw_ordinal,
                context_index,
            },
        );
        Ok(())
    }

    pub(crate) fn on_non_toolcall_msg(
        &mut self,
        raw_ordinal: u64,
        context_index: usize,
        item: &ResponseItem,
    ) -> Result<(), SpineError> {
        if !self.jit_enabled {
            return Ok(());
        }
        if tool_request_call_id(item).is_some()
            || tool_response_call_id(item).is_some()
            || matches!(
                item,
                ResponseItem::ToolSearchOutput { call_id: None, .. }
                    | ResponseItem::ToolSearchCall { call_id: None, .. }
            )
        {
            return Err(SpineError::InvalidEvent(
                "on_non_toolcall_msg received toolcall item".to_string(),
            ));
        }
        let context_index = u64::try_from(context_index)
            .map_err(|_| SpineError::InvalidEvent("context index overflow".to_string()))?;
        let lexed = lex_observed_msg(raw_ordinal, context_index, item, self.next_user_anchor)?;
        self.next_user_anchor = lexed.next_user_anchor;
        self.append_and_shift_msg(lexed.batch)
    }

    #[cfg(test)]
    pub(crate) fn observe_completed_toolcall(
        &mut self,
        toolcall: CompletedToolCall,
    ) -> Result<(), SpineError> {
        self.observe_completed_toolcall_with_raw_items(toolcall, &[])
    }

    pub(crate) fn observe_completed_toolcall_with_raw_items(
        &mut self,
        toolcall: CompletedToolCall,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<(), SpineError> {
        if !self.jit_enabled {
            return self.observe_completed_toolcall_for_trim(toolcall, raw_items);
        }
        let (event, token) = self.completed_toolcall_parts(&toolcall)?;
        let toolcall_seq = self.append_cached_event(event)?;
        self.push_completed_toolcall_token(token)?;
        self.append_trim_candidates_for_completed_toolcall(&toolcall, toolcall_seq, raw_items)?;
        self.clear_completed_toolcall_anchors(&toolcall);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn abort_pending_and_observe_completed_toolcall(
        &mut self,
        call_id: &str,
        toolcall: CompletedToolCall,
    ) -> Result<bool, SpineError> {
        self.abort_pending_and_observe_completed_toolcall_with_raw_items(call_id, toolcall, &[])
    }

    #[cfg(test)]
    pub(crate) fn abort_pending_and_observe_completed_toolcall_with_raw_items(
        &mut self,
        call_id: &str,
        toolcall: CompletedToolCall,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<bool, SpineError> {
        self.ensure_jit_enabled("Spine pending toolcall abort")?;
        if self
            .pending
            .as_ref()
            .is_none_or(|pending| pending.call_id() != call_id)
        {
            return Ok(false);
        }
        let (event, token) = self.completed_toolcall_parts(&toolcall)?;
        let toolcall_seq = self.append_event_after_staged_toolcall_shift(event, token)?;
        self.pending = None;
        self.append_trim_candidates_for_completed_toolcall(&toolcall, toolcall_seq, raw_items)?;
        self.clear_completed_toolcall_anchors(&toolcall);
        Ok(true)
    }

    #[cfg(test)]
    pub(crate) fn commit_completed_toolcall_as_ordinary_with_raw_items(
        &mut self,
        call_id: &str,
        toolcall: CompletedToolCall,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<bool, SpineError> {
        self.ensure_jit_enabled("Spine ordinary toolcall commit")?;
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.call_id() == call_id)
        {
            return self.abort_pending_and_observe_completed_toolcall_with_raw_items(
                call_id, toolcall, raw_items,
            );
        }
        let (event, token) = self.completed_toolcall_parts(&toolcall)?;
        let toolcall_seq = self.append_event_after_staged_toolcall_shift(event, token)?;
        self.append_trim_candidates_for_completed_toolcall(&toolcall, toolcall_seq, raw_items)?;
        self.clear_completed_toolcall_anchors(&toolcall);
        Ok(false)
    }

    #[cfg(test)]
    pub(crate) fn observe_recorded_tool_output_group_as_completed_toolcall(
        &mut self,
        tool_responses: &[(String, u64, usize)],
    ) -> Result<(), SpineError> {
        self.observe_recorded_tool_output_group_as_completed_toolcall_with_raw_items(
            tool_responses,
            &[],
        )
    }

    pub(crate) fn observe_recorded_tool_output_group_as_completed_toolcall_with_raw_items(
        &mut self,
        tool_responses: &[(String, u64, usize)],
        raw_items: &[Option<ResponseItem>],
    ) -> Result<(), SpineError> {
        if !self.jit_enabled {
            return self.observe_recorded_tool_output_group_for_trim(tool_responses, raw_items);
        }
        let mut response_segments = Vec::new();
        let mut request_call_ids = Vec::new();
        for (call_id, raw_ordinal, context_index) in tool_responses {
            if self.control_call_ids.contains(call_id)
                || self
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.call_id() == call_id)
            {
                continue;
            }
            if !request_call_ids.contains(call_id) {
                if !self.ordinary_tool_requests.contains_key(call_id) {
                    continue;
                }
                request_call_ids.push(call_id.clone());
            }
            response_segments.push(CompletedToolCallSegment {
                kind: ToolCallSegmentKind::Response,
                raw_ordinal: *raw_ordinal,
                context_index: *context_index,
            });
        }
        if request_call_ids.is_empty() || response_segments.is_empty() {
            return Ok(());
        }
        request_call_ids.sort_by(|left, right| {
            let left_anchor = self.ordinary_tool_requests.get(left);
            let right_anchor = self.ordinary_tool_requests.get(right);
            left_anchor
                .map(|anchor| (anchor.context_index, anchor.raw_ordinal))
                .cmp(&right_anchor.map(|anchor| (anchor.context_index, anchor.raw_ordinal)))
                .then_with(|| left.cmp(right))
        });
        let mut segments = Vec::with_capacity(request_call_ids.len() + response_segments.len());
        for request_call_id in &request_call_ids {
            let request = self
                .ordinary_tool_requests
                .get(request_call_id)
                .ok_or_else(|| {
                    SpineError::InvalidEvent(format!(
                        "missing tool request anchor for call_id={request_call_id}"
                    ))
                })?;
            segments.push(CompletedToolCallSegment {
                kind: ToolCallSegmentKind::Request,
                raw_ordinal: request.raw_ordinal,
                context_index: usize::try_from(request.context_index).map_err(|_| {
                    SpineError::InvalidEvent("tool request context index overflow".to_string())
                })?,
            });
        }
        response_segments.sort_by_key(|segment| (segment.context_index, segment.raw_ordinal));
        segments.extend(response_segments);
        let call_id = request_call_ids.first().cloned().ok_or_else(|| {
            SpineError::InvalidEvent("completed toolcall missing call id".to_string())
        })?;
        self.observe_completed_toolcall_with_raw_items(
            CompletedToolCall {
                call_id,
                request_call_ids,
                segments,
            },
            raw_items,
        )
    }

    pub(super) fn completed_toolcall_parts(
        &self,
        toolcall: &CompletedToolCall,
    ) -> Result<(SpineLedgerEvent, SpineToken), SpineError> {
        let plan = plan_control_toolcall(ControlIntent::Ordinary);
        debug_assert_eq!(plan.token_sequence(), &[LexedTokenKind::ToolCall]);
        lex_toolcall_event_token(
            toolcall.segments.iter().copied(),
            Some(toolcall.request_call_ids.len()),
        )
    }

    pub(super) fn clear_completed_toolcall_anchors(&mut self, toolcall: &CompletedToolCall) {
        for request_call_id in &toolcall.request_call_ids {
            self.open_requests.remove(request_call_id);
            self.ordinary_tool_requests.remove(request_call_id);
        }
        self.open_requests.remove(&toolcall.call_id);
        self.ordinary_tool_requests.remove(&toolcall.call_id);
        #[cfg(test)]
        {
            for request_call_id in &toolcall.request_call_ids {
                self.pending_tool_responses.remove(request_call_id);
            }
            self.pending_tool_responses.remove(&toolcall.call_id);
        }
        for request_call_id in &toolcall.request_call_ids {
            self.tree_call_ids.remove(request_call_id);
            self.control_call_ids.remove(request_call_id);
        }
        self.tree_call_ids.remove(&toolcall.call_id);
        self.control_call_ids.remove(&toolcall.call_id);
    }

    pub(super) fn remap_completed_toolcall_context_indices(
        &self,
        mut toolcall: CompletedToolCall,
        toolcall_context_start: usize,
    ) -> Result<CompletedToolCall, SpineError> {
        let mut context_index = toolcall_context_start;
        for segment in &mut toolcall.segments {
            segment.context_index = context_index;
            context_index = context_index.checked_add(1).ok_or_else(|| {
                SpineError::InvalidEvent("toolcall context index overflow".to_string())
            })?;
        }
        Ok(toolcall)
    }

    fn push_completed_toolcall_token(&mut self, token: SpineToken) -> Result<(), SpineError> {
        let staged = self.parser.staged_after_token(token, &self.archive())?;
        self.parser.install_staged(staged);
        Ok(())
    }

    #[cfg(test)]
    fn append_event_after_staged_toolcall_shift(
        &mut self,
        event: SpineLedgerEvent,
        token: SpineToken,
    ) -> Result<u64, SpineError> {
        let staged_parse_stack = self.parser.staged_after_token(token, &self.archive())?;
        let event_seq = self.append_cached_event(event)?;
        self.parser
            .replace_parse_stack_for_runtime_transition(staged_parse_stack);
        Ok(event_seq)
    }

    fn append_and_shift_msg(
        &mut self,
        lexed: crate::spine::lexer::LexedTokenBatch,
    ) -> Result<(), SpineError> {
        let staged = self
            .parser
            .staged_after_lexed_batch_for_observe(&lexed, &self.archive())?;
        for event in lexed.events {
            self.append_cached_event(event)?;
        }
        self.parser.install_staged(staged);
        Ok(())
    }
}
