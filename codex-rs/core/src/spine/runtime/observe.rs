use codex_protocol::models::ResponseItem;

use super::CompletedToolCall;
use super::CompletedToolCallSegment;
use super::OpenRequestAnchor;
use super::PendingToolRequest;
use super::PendingToolResponse;
use super::SPINE_NAMESPACE;
use super::SPINE_TOOL_OPEN;
use super::SPINE_TOOL_TREE;
use super::SpineError;
use super::SpineRuntime;
use super::support::is_spine_parser_control_tool_name;
use super::support::tool_request_call_id;
use super::support::tool_response_call_id;
use crate::spine::CHECKPOINT_VERSION;
use crate::spine::checkpoint::SpineCheckpoint;
use crate::spine::io::hash_raw_live;
use crate::spine::lexer::LexedTokenBatch;
use crate::spine::lexer::lex_observed_msg;
use crate::spine::lexer::lex_toolcall;
use crate::spine::model::ToolCallEventSegment;
use crate::spine::model::ToolCallSegmentKind;
use crate::spine::model::TrimBodyUpdate;

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
        let is_tool_request = tool_request_call_id(item).is_some();
        if is_tool_request {
            self.restore_live_rollback_if_needed(raw_ordinal, context_index)?;
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
        self.restore_live_rollback_if_needed(raw_ordinal, context_index)?;
        self.on_non_toolcall_msg(raw_ordinal, context_index, item)
    }

    fn restore_live_rollback_if_needed(
        &mut self,
        raw_ordinal: u64,
        context_index: usize,
    ) -> Result<(), SpineError> {
        let Some(last_context_index) = self.parser.last_visible_response_context_index() else {
            return Ok(());
        };
        if context_index != last_context_index {
            return Ok(());
        }
        let checkpoint = self
            .store
            .checkpoints()?
            .into_iter()
            .filter(|checkpoint| checkpoint.context_len == context_index)
            .filter(|checkpoint| checkpoint.raw_ordinal <= raw_ordinal)
            .max_by_key(|checkpoint| (checkpoint.raw_ordinal, checkpoint.token_seq));
        let Some(checkpoint) = checkpoint else {
            return Err(SpineError::InvalidStore(format!(
                "missing spine live rollback checkpoint before mutable context_index {context_index}"
            )));
        };
        self.restore_live_rollback_checkpoint(raw_ordinal, &checkpoint)
    }

    fn restore_live_rollback_checkpoint(
        &mut self,
        raw_ordinal: u64,
        checkpoint: &SpineCheckpoint,
    ) -> Result<(), SpineError> {
        if checkpoint.version != CHECKPOINT_VERSION {
            return Err(SpineError::InvalidStore(format!(
                "unsupported spine checkpoint version {}",
                checkpoint.version
            )));
        }
        let cut = usize::try_from(checkpoint.raw_ordinal)
            .map_err(|_| SpineError::InvalidEvent("checkpoint raw ordinal overflow".to_string()))?;
        let raw_index = usize::try_from(raw_ordinal)
            .map_err(|_| SpineError::InvalidEvent("raw ordinal overflow".to_string()))?;
        if cut > raw_index {
            return Err(SpineError::InvalidStore(format!(
                "spine live rollback checkpoint raw ordinal {} is after observed raw ordinal {}",
                checkpoint.raw_ordinal, raw_ordinal
            )));
        }
        if raw_index >= self.raw_live.len() {
            return Err(SpineError::InvalidEvent(format!(
                "observed raw ordinal {raw_ordinal} exceeds raw_live length {}",
                self.raw_live.len()
            )));
        }
        for slot in &mut self.raw_live[cut..raw_index] {
            *slot = false;
        }
        self.raw_live[raw_index] = true;
        if checkpoint.raw_live_hash != hash_raw_live(&self.raw_live[..cut]) {
            return Err(SpineError::InvalidStore(format!(
                "spine live rollback checkpoint raw_live hash mismatch for {}",
                checkpoint.checkpoint_id
            )));
        }
        self.ledger
            .retain_trim_events_at_or_before(checkpoint.trim_seq_watermark);
        self.parser.restore_from_checkpoint(checkpoint);
        self.clear_turn_local_observation_state();
        Ok(())
    }

    fn clear_turn_local_observation_state(&mut self) {
        self.open_requests.clear();
        self.control_call_ids.clear();
        self.tree_call_ids.clear();
        self.ordinary_tool_requests.clear();
        self.pending_tool_responses.clear();
        self.pending = None;
        #[cfg(test)]
        self.control_receipts.clear();
        self.pending_memory_context_accounting = None;
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
        let context_index = u64::try_from(_context_index)
            .map_err(|_| SpineError::InvalidEvent("context index overflow".to_string()))?;
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
        self.flush_pending_tool_responses_before_non_toolcall()?;
        if self.has_pending_tool_request() {
            return Err(SpineError::InvalidEvent(format!(
                "cannot observe non-toolcall raw_ordinal={raw_ordinal} while durable tool requests are pending"
            )));
        }
        let context_index = u64::try_from(context_index)
            .map_err(|_| SpineError::InvalidEvent("context index overflow".to_string()))?;
        let lexed = lex_observed_msg(raw_ordinal, context_index, item, self.next_user_anchor)?;
        self.next_user_anchor = lexed.next_user_anchor;
        self.append_and_install_observed_msg(lexed.batch)
    }

    fn flush_pending_tool_responses_before_non_toolcall(
        &mut self,
    ) -> Result<Vec<TrimBodyUpdate>, SpineError> {
        if self.pending_tool_responses.is_empty() {
            return Ok(Vec::new());
        }
        let mut tool_responses = Vec::new();
        for (call_id, responses) in &self.pending_tool_responses {
            if self.control_call_ids.contains(call_id)
                || self.tree_call_ids.contains(call_id)
                || self
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.call_id() == call_id)
            {
                continue;
            }
            if !self.ordinary_tool_requests.contains_key(call_id) {
                continue;
            }
            for response in responses {
                tool_responses.push((
                    call_id.clone(),
                    response.raw_ordinal,
                    usize::try_from(response.context_index).map_err(|_| {
                        SpineError::InvalidEvent("tool response context index overflow".to_string())
                    })?,
                ));
            }
        }
        if tool_responses.is_empty() {
            return Ok(Vec::new());
        }
        tool_responses
            .sort_by_key(|(_, raw_ordinal, context_index)| (*context_index, *raw_ordinal));
        self.observe_recorded_tool_output_group_as_completed_toolcall_with_raw_items(
            &tool_responses,
            &[],
        )
    }

    #[cfg(test)]
    pub(crate) fn observe_completed_toolcall(
        &mut self,
        toolcall: CompletedToolCall,
    ) -> Result<(), SpineError> {
        self.observe_completed_toolcall_with_raw_items(toolcall, &[])
            .map(|_| ())
    }

    pub(crate) fn observe_completed_toolcall_with_raw_items(
        &mut self,
        toolcall: CompletedToolCall,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<Vec<TrimBodyUpdate>, SpineError> {
        if !self.jit_enabled {
            return self.observe_completed_toolcall_for_trim(toolcall, raw_items);
        }
        let lexed = self.completed_toolcall_batch(&toolcall)?;
        let toolcall_seq = self.append_and_install_observed_toolcall(lexed)?;
        self.finish_observed_completed_toolcall(&toolcall, toolcall_seq, raw_items)
    }

    pub(crate) fn abort_pending_and_observe_completed_toolcall_with_raw_items(
        &mut self,
        call_id: &str,
        toolcall: CompletedToolCall,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<(bool, Vec<TrimBodyUpdate>), SpineError> {
        self.ensure_jit_enabled("Spine pending toolcall abort")?;
        if self
            .pending
            .as_ref()
            .is_none_or(|pending| pending.call_id() != call_id)
        {
            return Ok((false, Vec::new()));
        }
        let lexed = self.completed_toolcall_batch(&toolcall)?;
        let toolcall_seq = self.append_and_install_observed_toolcall(lexed)?;
        self.pending = None;
        let updates =
            self.finish_observed_completed_toolcall(&toolcall, toolcall_seq, raw_items)?;
        Ok((true, updates))
    }

    pub(crate) fn commit_completed_toolcall_as_ordinary_with_raw_items(
        &mut self,
        call_id: &str,
        toolcall: CompletedToolCall,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<(bool, Vec<TrimBodyUpdate>), SpineError> {
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
        let lexed = self.completed_toolcall_batch(&toolcall)?;
        let toolcall_seq = self.append_and_install_observed_toolcall(lexed)?;
        self.control_call_ids.remove(call_id);
        self.open_requests.remove(call_id);
        let updates =
            self.finish_observed_completed_toolcall(&toolcall, toolcall_seq, raw_items)?;
        Ok((false, updates))
    }

    fn failed_function_tool_output_call_id(item: &ResponseItem) -> Option<&str> {
        let ResponseItem::FunctionCallOutput { call_id, output } = item else {
            return None;
        };
        if output.success == Some(false) {
            Some(call_id)
        } else {
            None
        }
    }

    pub(crate) fn observe_recorded_tool_output_group_as_completed_toolcall_with_raw_items(
        &mut self,
        tool_responses: &[(String, u64, usize)],
        raw_items: &[Option<ResponseItem>],
    ) -> Result<Vec<TrimBodyUpdate>, SpineError> {
        if !self.jit_enabled {
            return self.observe_recorded_tool_output_group_for_trim(tool_responses, raw_items);
        }
        let mut response_segments = Vec::new();
        let mut request_call_ids = Vec::new();
        for (call_id, raw_ordinal, context_index) in tool_responses {
            let output_failed = raw_items
                .get(usize::try_from(*raw_ordinal).map_err(|_| {
                    SpineError::InvalidEvent("tool response raw ordinal overflow".to_string())
                })?)
                .and_then(Option::as_ref)
                .and_then(Self::failed_function_tool_output_call_id)
                .is_some_and(|output_call_id| output_call_id == call_id);
            if !output_failed
                && (self.control_call_ids.contains(call_id)
                    || self
                        .pending
                        .as_ref()
                        .is_some_and(|pending| pending.call_id() == call_id))
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
            return Ok(Vec::new());
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

    pub(super) fn completed_toolcall_batch(
        &self,
        toolcall: &CompletedToolCall,
    ) -> Result<LexedTokenBatch, SpineError> {
        let segments = toolcall
            .segments
            .iter()
            .map(|segment| {
                Ok(ToolCallEventSegment {
                    kind: segment.kind,
                    raw_ordinal: segment.raw_ordinal,
                    context_index: u64::try_from(segment.context_index).map_err(|_| {
                        SpineError::InvalidEvent("toolcall context index overflow".to_string())
                    })?,
                })
            })
            .collect::<Result<Vec<_>, SpineError>>()?;
        lex_toolcall(segments, Some(toolcall.request_call_ids.len()))
    }

    pub(super) fn clear_completed_toolcall_anchors(&mut self, toolcall: &CompletedToolCall) {
        for request_call_id in &toolcall.request_call_ids {
            self.open_requests.remove(request_call_id);
            self.ordinary_tool_requests.remove(request_call_id);
        }
        self.open_requests.remove(&toolcall.call_id);
        self.ordinary_tool_requests.remove(&toolcall.call_id);
        for request_call_id in &toolcall.request_call_ids {
            self.pending_tool_responses.remove(request_call_id);
        }
        self.pending_tool_responses.remove(&toolcall.call_id);
        for request_call_id in &toolcall.request_call_ids {
            self.tree_call_ids.remove(request_call_id);
            self.control_call_ids.remove(request_call_id);
        }
        self.tree_call_ids.remove(&toolcall.call_id);
        self.control_call_ids.remove(&toolcall.call_id);
    }

    fn finish_observed_completed_toolcall(
        &mut self,
        toolcall: &CompletedToolCall,
        toolcall_seq: u64,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<Vec<TrimBodyUpdate>, SpineError> {
        let updates =
            self.append_trim_candidates_for_completed_toolcall(toolcall, toolcall_seq, raw_items)?;
        self.clear_completed_toolcall_anchors(toolcall);
        Ok(updates)
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

    fn append_and_install_observed_msg(
        &mut self,
        lexed: LexedTokenBatch,
    ) -> Result<(), SpineError> {
        self.append_and_install_lexed_batch(lexed).map(|_| ())
    }

    fn append_and_install_observed_toolcall(
        &mut self,
        lexed: LexedTokenBatch,
    ) -> Result<u64, SpineError> {
        self.append_and_install_lexed_batch(lexed)
    }

    fn append_and_install_lexed_batch(
        &mut self,
        lexed: LexedTokenBatch,
    ) -> Result<u64, SpineError> {
        let parser_install = self.parser.consume_lexed_batch(&lexed, &self.archive())?;
        let mut event_seq = None;
        for event in lexed.events {
            event_seq = Some(self.append_cached_event(event)?);
        }
        self.parser.install_prepared_observe(parser_install);
        event_seq.ok_or_else(|| SpineError::Invariant("lexer produced no event".to_string()))
    }
}
