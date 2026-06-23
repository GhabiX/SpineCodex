use crate::context_manager::estimate_response_item_model_visible_bytes;
use crate::spine::io::hash_raw_live;
use crate::spine::model::LoggedTrimEvent;
use crate::spine::model::RawMask;
use crate::spine::model::TOOL_RESPONSE_TRIM_THRESHOLD_BYTES;
use crate::spine::model::TrimEvent;
use crate::spine::model::TrimProjection;
use crate::spine::model::TrimResponseKind;
use crate::spine::model::TrimSliceSpec;
use crate::spine::model::TrimTarget;
use crate::spine::model::TrimTargetState;
use crate::spine::render::matched_tool_output;
use crate::spine::render::trim_body_error;
use crate::spine::runtime::CompletedToolCall;
use crate::spine::runtime::SpineError;
use crate::spine::runtime::SpineTrimOutcome;
use crate::spine::store::SpineStore;
use codex_protocol::models::ResponseItem;

pub(super) struct Trimmer<'a> {
    store: &'a SpineStore,
    trim_events: &'a mut Vec<LoggedTrimEvent>,
    next_trim_seq: &'a mut u64,
    raw_len: u64,
    raw_live: &'a [bool],
    trim_enabled: bool,
}

impl<'a> Trimmer<'a> {
    pub(super) fn new(
        store: &'a SpineStore,
        trim_events: &'a mut Vec<LoggedTrimEvent>,
        next_trim_seq: &'a mut u64,
        raw_len: u64,
        raw_live: &'a [bool],
        trim_enabled: bool,
    ) -> Self {
        Self {
            store,
            trim_events,
            next_trim_seq,
            raw_len,
            raw_live,
            trim_enabled,
        }
    }

    pub(super) fn on_tool_call(
        &mut self,
        toolcall: &CompletedToolCall,
        toolcall_seq: u64,
        raw_items: &[Option<ResponseItem>],
        record_boundary: bool,
    ) -> Result<(), SpineError> {
        if !self.trim_enabled {
            return Ok(());
        }
        if record_boundary {
            self.append_event(TrimEvent::ToolCallBoundary {
                toolcall_seq,
                raw_boundary: self.raw_len,
                raw_live_hash: hash_raw_live(self.raw_live),
            })?;
        }
        if raw_items.is_empty() {
            return Ok(());
        }
        let projection_seq = toolcall_seq.checked_add(1).ok_or_else(|| {
            SpineError::InvalidEvent("spine trim toolcall seq overflow".to_string())
        })?;
        let existing_projection = self.current_projection(projection_seq)?;
        for segment in &toolcall.segments {
            if segment.kind != crate::spine::model::ToolCallSegmentKind::Response
                || existing_projection
                    .contains_toolcall_raw_target(toolcall_seq, segment.raw_ordinal)
            {
                continue;
            }
            let raw_index = trim_raw_ordinal_usize(segment.raw_ordinal)?;
            let Some(item) = raw_items.get(raw_index).and_then(Option::as_ref) else {
                continue;
            };
            let Some((response_kind, call_id)) = trimmable_text_tool_response(item) else {
                continue;
            };
            let original_visible_size = estimate_response_item_model_visible_bytes(item);
            if original_visible_size <= TOOL_RESPONSE_TRIM_THRESHOLD_BYTES {
                continue;
            }
            let trim_id = format!("trim_{}", *self.next_trim_seq);
            self.append_event(TrimEvent::Candidate {
                trim_id,
                toolcall_seq,
                raw_ordinal: segment.raw_ordinal,
                context_index: segment.context_index,
                call_id: call_id.to_string(),
                response_kind,
                original_visible_size,
            })?;
        }
        Ok(())
    }

    pub(super) fn snip(
        &mut self,
        trim_id: &str,
        latest_live_completed_toolcall_seq: Option<u64>,
        current_structural_seq: u64,
    ) -> Result<SpineTrimOutcome, SpineError> {
        let trim_id = trim_id.trim();
        let Some(target) = self.target_in_latest_toolcall(
            trim_id,
            latest_live_completed_toolcall_seq,
            current_structural_seq,
        )?
        else {
            return Ok(SpineTrimOutcome::miss(trim_id));
        };
        if matches!(target.state, TrimTargetState::Snipped) {
            return Ok(SpineTrimOutcome::already_cleared(trim_id));
        }
        self.append_event(TrimEvent::Snipped {
            trim_id: trim_id.to_string(),
            raw_boundary: self.raw_len,
            raw_live_hash: hash_raw_live(self.raw_live),
        })?;
        Ok(SpineTrimOutcome::cleared(trim_id))
    }

    pub(super) fn slice(
        &mut self,
        trim_id: &str,
        slice: TrimSliceSpec,
        latest_live_completed_toolcall_seq: Option<u64>,
        current_structural_seq: u64,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<SpineTrimOutcome, SpineError> {
        let trim_id = trim_id.trim();
        let Some(target) = self.target_in_latest_toolcall(
            trim_id,
            latest_live_completed_toolcall_seq,
            current_structural_seq,
        )?
        else {
            return Ok(SpineTrimOutcome::miss(trim_id));
        };
        let current_body = current_visible_body(&target, raw_items)?;
        let Some(visible_body) = apply_slice(&current_body, &slice) else {
            return Ok(SpineTrimOutcome::miss(trim_id));
        };
        self.append_event(TrimEvent::Sliced {
            trim_id: trim_id.to_string(),
            raw_boundary: self.raw_len,
            raw_live_hash: hash_raw_live(self.raw_live),
            slice,
            visible_body,
        })?;
        Ok(SpineTrimOutcome::sliced(trim_id))
    }

    pub(super) fn current_projection(
        &self,
        current_structural_seq: u64,
    ) -> Result<TrimProjection, SpineError> {
        if !self.trim_enabled {
            return Ok(TrimProjection::default());
        }
        trim_projection_from_events(
            self.trim_events,
            self.raw_live,
            current_structural_seq,
            None,
        )
    }

    fn target_in_latest_toolcall(
        &self,
        trim_id: &str,
        latest_live_completed_toolcall_seq: Option<u64>,
        current_structural_seq: u64,
    ) -> Result<Option<TrimTarget>, SpineError> {
        if trim_id.is_empty() {
            return Ok(None);
        }
        let projection = self.current_projection(current_structural_seq)?;
        let Some(target) = projection.target(trim_id) else {
            return Ok(None);
        };
        if Some(target.toolcall_seq) != latest_live_completed_toolcall_seq {
            return Ok(None);
        }
        Ok(Some(target.clone()))
    }

    fn append_event(&mut self, event: TrimEvent) -> Result<u64, SpineError> {
        let trim_seq = *self.next_trim_seq;
        let next_trim_seq = trim_seq
            .checked_add(1)
            .ok_or_else(|| SpineError::InvalidEvent("spine trim seq overflow".to_string()))?;
        let logged = LoggedTrimEvent { trim_seq, event };
        self.store.append_logged_trim_event(&logged)?;
        self.trim_events.push(logged);
        *self.next_trim_seq = next_trim_seq;
        Ok(trim_seq)
    }
}

pub(super) fn trim_projection_from_events(
    events: &[LoggedTrimEvent],
    raw_live: &[bool],
    current_structural_seq: u64,
    trim_seq_watermark: Option<u64>,
) -> Result<TrimProjection, SpineError> {
    let raw_mask = RawMask::new(raw_live);
    let mut projection = TrimProjection::default();
    let mut events = events.iter().collect::<Vec<_>>();
    events.sort_by_key(|event| event.trim_seq);
    for event in events {
        if trim_seq_watermark.is_some_and(|watermark| event.trim_seq > watermark) {
            continue;
        }
        if !event.allowed_by(raw_mask)? {
            continue;
        }
        match &event.event {
            TrimEvent::ToolCallBoundary { .. } => {}
            TrimEvent::Candidate {
                trim_id,
                toolcall_seq,
                raw_ordinal,
                context_index,
                call_id,
                response_kind,
                original_visible_size,
            } => {
                if *toolcall_seq >= current_structural_seq {
                    continue;
                }
                projection.insert_candidate(TrimTarget {
                    trim_id: trim_id.clone(),
                    toolcall_seq: *toolcall_seq,
                    raw_ordinal: *raw_ordinal,
                    context_index: *context_index,
                    call_id: call_id.clone(),
                    response_kind: *response_kind,
                    original_visible_size: *original_visible_size,
                    state: TrimTargetState::Tagged,
                });
            }
            TrimEvent::Cleared { trim_id, .. } | TrimEvent::Snipped { trim_id, .. } => {
                projection.mark_snipped(trim_id);
            }
            TrimEvent::Sliced {
                trim_id,
                visible_body,
                ..
            } => {
                projection.mark_sliced(trim_id, visible_body.clone());
            }
        }
    }
    Ok(projection)
}

pub(super) fn trimmable_text_tool_response(
    item: &ResponseItem,
) -> Option<(TrimResponseKind, &str)> {
    match item {
        ResponseItem::FunctionCallOutput { call_id, output } if output.text_content().is_some() => {
            Some((TrimResponseKind::FunctionCallOutput, call_id.as_str()))
        }
        ResponseItem::CustomToolCallOutput {
            call_id, output, ..
        } if output.text_content().is_some() => {
            Some((TrimResponseKind::CustomToolCallOutput, call_id.as_str()))
        }
        _ => None,
    }
}

pub(super) fn current_visible_body(
    target: &TrimTarget,
    raw_items: &[Option<ResponseItem>],
) -> Result<String, SpineError> {
    match &target.state {
        TrimTargetState::Tagged => {
            let raw_index = trim_raw_ordinal_usize(target.raw_ordinal)?;
            let item = raw_items
                .get(raw_index)
                .and_then(Option::as_ref)
                .ok_or_else(|| {
                    SpineError::InvalidEvent(format!(
                        "missing raw item for trim target {}",
                        target.trim_id
                    ))
                })?;
            let Some((response_kind, _)) = trimmable_text_tool_response(item) else {
                return Err(trim_body_error(target));
            };
            if response_kind != target.response_kind {
                return Err(SpineError::SidecarCorruption(format!(
                    "trim target {} response kind mismatch",
                    target.trim_id
                )));
            }
            let text = matched_tool_output(item, target, "visible raw item")?
                .text_content()
                .map(str::to_string)
                .ok_or_else(|| trim_body_error(target))?;
            Ok(strip_trim_tag_prefix(&text, &target.trim_id))
        }
        TrimTargetState::Sliced { visible_body } => Ok(visible_body.clone()),
        TrimTargetState::Snipped => {
            Ok(crate::spine::model::TOOL_RESULT_CLEARED_MESSAGE.to_string())
        }
    }
}

fn strip_trim_tag_prefix(text: &str, trim_id: &str) -> String {
    let tag = format!("[TRIM_ID: {trim_id}]\n");
    text.strip_prefix(&tag).unwrap_or(text).to_string()
}

fn trim_raw_ordinal_usize(raw_ordinal: u64) -> Result<usize, SpineError> {
    usize::try_from(raw_ordinal)
        .map_err(|_| SpineError::InvalidEvent("trim raw ordinal overflow".to_string()))
}

fn apply_slice(text: &str, slice: &TrimSliceSpec) -> Option<String> {
    match slice {
        TrimSliceSpec::Head { head } => Some(text[..prefix_byte_index(text, *head)].to_string()),
        TrimSliceSpec::Tail { tail } => {
            if *tail == 0 {
                Some(String::new())
            } else {
                let keep_from = text.chars().count().saturating_sub(*tail);
                Some(text[prefix_byte_index(text, keep_from)..].to_string())
            }
        }
        TrimSliceSpec::Anchor {
            anchor,
            preceding,
            following,
        } => {
            if anchor.is_empty() {
                return None;
            }
            let anchor_start = text.find(anchor)?;
            let anchor_end = anchor_start + anchor.len();
            let start = byte_index_before_chars(text, anchor_start, *preceding);
            let end = byte_index_after_chars(text, anchor_end, *following);
            Some(text[start..end].to_string())
        }
    }
}

fn prefix_byte_index(text: &str, count: usize) -> usize {
    text.char_indices()
        .nth(count)
        .map(|(idx, _)| idx)
        .unwrap_or(text.len())
}

fn byte_index_before_chars(text: &str, byte_index: usize, count: usize) -> usize {
    let prefix = &text[..byte_index];
    let total = prefix.chars().count();
    let keep = total.saturating_sub(count);
    prefix
        .char_indices()
        .nth(keep)
        .map(|(idx, _)| idx)
        .unwrap_or(byte_index)
}

fn byte_index_after_chars(text: &str, byte_index: usize, count: usize) -> usize {
    if count == 0 {
        return byte_index;
    }
    let suffix = &text[byte_index..];
    match suffix.char_indices().nth(count) {
        Some((idx, _)) => byte_index + idx,
        None => text.len(),
    }
}
