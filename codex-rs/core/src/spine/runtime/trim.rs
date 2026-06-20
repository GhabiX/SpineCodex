use codex_protocol::models::ResponseItem;

use super::CompletedToolCall;
use super::CompletedToolCallSegment;
use super::SpineError;
use super::SpineRuntime;
use super::SpineTrimOutcome;
use crate::spine::model::RawMask;
use crate::spine::model::SpineLedgerEvent;
use crate::spine::model::ToolCallSegmentKind;
use crate::spine::model::TrimEvent;
use crate::spine::model::TrimProjection;
use crate::spine::model::TrimSliceSpec;
use crate::spine::render::project_raw_history_with_trim_projection;
use crate::spine::trimmer::Trimmer;
use crate::spine::trimmer::trim_projection_from_events;

impl SpineRuntime {
    pub(crate) fn set_trim_enabled(&mut self, enabled: bool) {
        self.trim_enabled = enabled;
    }

    fn trimmer(&mut self) -> Trimmer<'_> {
        Trimmer::new(
            &self.store,
            &mut self.ledger.trim_events,
            &mut self.ledger.next_trim_seq,
            self.raw_len,
            &self.raw_live,
            self.trim_enabled,
        )
    }

    fn current_trim_structural_seq(&self) -> u64 {
        if self.jit_enabled {
            self.ledger.next_event_seq
        } else {
            self.ledger.next_trim_seq
        }
    }

    pub(super) fn observe_recorded_tool_output_group_for_trim(
        &mut self,
        tool_responses: &[(String, u64, usize)],
        raw_items: &[Option<ResponseItem>],
    ) -> Result<(), SpineError> {
        let mut segments = Vec::new();
        let mut request_call_ids = Vec::new();
        for (call_id, raw_ordinal, context_index) in tool_responses {
            if !request_call_ids.contains(call_id) {
                request_call_ids.push(call_id.clone());
            }
            segments.push(CompletedToolCallSegment {
                kind: ToolCallSegmentKind::Response,
                raw_ordinal: *raw_ordinal,
                context_index: *context_index,
            });
        }
        if request_call_ids.is_empty() || segments.is_empty() {
            return Ok(());
        }
        segments.sort_by_key(|segment| (segment.context_index, segment.raw_ordinal));
        let call_id = request_call_ids.first().cloned().ok_or_else(|| {
            SpineError::InvalidEvent("completed trim toolcall missing call id".to_string())
        })?;
        self.observe_completed_toolcall_for_trim(
            CompletedToolCall {
                call_id,
                request_call_ids,
                segments,
            },
            raw_items,
        )
    }

    pub(super) fn append_trim_candidates_for_completed_toolcall(
        &mut self,
        toolcall: &CompletedToolCall,
        toolcall_seq: u64,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<(), SpineError> {
        self.trimmer()
            .on_tool_call(toolcall, toolcall_seq, raw_items, false)
    }

    pub(super) fn observe_completed_toolcall_for_trim(
        &mut self,
        toolcall: CompletedToolCall,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<(), SpineError> {
        let toolcall_seq = self.ledger.next_trim_seq;
        self.trimmer()
            .on_tool_call(&toolcall, toolcall_seq, raw_items, true)
    }

    pub(super) fn current_trim_projection(&self) -> Result<TrimProjection, SpineError> {
        if !self.trim_enabled {
            return Ok(TrimProjection::default());
        }
        trim_projection_from_events(
            &self.ledger.trim_events,
            &self.raw_live,
            self.current_trim_structural_seq(),
            None,
        )
    }

    fn latest_live_completed_toolcall_seq(&self) -> Result<Option<u64>, SpineError> {
        if !self.jit_enabled {
            return self.latest_live_trim_toolcall_seq();
        }
        let raw_mask = RawMask::new(&self.raw_live);
        for event in self.ledger.events.iter().rev() {
            if event.seq >= self.ledger.next_event_seq {
                continue;
            }
            if matches!(event.event, SpineLedgerEvent::ToolCall { .. })
                && event.allowed_by(raw_mask)?
            {
                return Ok(Some(event.seq));
            }
        }
        Ok(None)
    }

    fn latest_live_trim_toolcall_seq(&self) -> Result<Option<u64>, SpineError> {
        let raw_mask = RawMask::new(&self.raw_live);
        for event in self.ledger.trim_events.iter().rev() {
            let TrimEvent::ToolCallBoundary { toolcall_seq, .. } = event.event else {
                continue;
            };
            if event.allowed_by(raw_mask)? {
                return Ok(Some(toolcall_seq));
            }
        }
        Ok(None)
    }

    pub(crate) fn trim_tool_response(
        &mut self,
        trim_id: &str,
    ) -> Result<SpineTrimOutcome, SpineError> {
        let latest = self.latest_live_completed_toolcall_seq()?;
        let current_seq = self.current_trim_structural_seq();
        self.trimmer().snip(trim_id, latest, current_seq)
    }

    pub(crate) fn slice_tool_response_head(
        &mut self,
        trim_id: &str,
        head: usize,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<SpineTrimOutcome, SpineError> {
        self.slice_tool_response(trim_id, TrimSliceSpec::Head { head }, raw_items)
    }

    pub(crate) fn slice_tool_response_tail(
        &mut self,
        trim_id: &str,
        tail: usize,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<SpineTrimOutcome, SpineError> {
        self.slice_tool_response(trim_id, TrimSliceSpec::Tail { tail }, raw_items)
    }

    pub(crate) fn slice_tool_response_anchor(
        &mut self,
        trim_id: &str,
        anchor: &str,
        preceding: usize,
        following: usize,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<SpineTrimOutcome, SpineError> {
        self.slice_tool_response(
            trim_id,
            TrimSliceSpec::Anchor {
                anchor: anchor.to_string(),
                preceding,
                following,
            },
            raw_items,
        )
    }

    fn slice_tool_response(
        &mut self,
        trim_id: &str,
        slice: TrimSliceSpec,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<SpineTrimOutcome, SpineError> {
        let latest = self.latest_live_completed_toolcall_seq()?;
        let current_seq = self.current_trim_structural_seq();
        self.trimmer()
            .slice(trim_id, slice, latest, current_seq, raw_items)
    }

    pub(crate) fn project_raw_history_with_trim(
        &self,
        raw_items: &[ResponseItem],
    ) -> Result<Vec<ResponseItem>, SpineError> {
        let trim_projection = self.current_trim_projection()?;
        project_raw_history_with_trim_projection(raw_items, &trim_projection)
    }
}
