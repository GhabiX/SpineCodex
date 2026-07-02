use crate::context_manager::estimate_response_item_model_visible_bytes;
use crate::spine::io::hash_raw_live;
use crate::spine::model::LoggedTrimEvent;
use crate::spine::model::RawMask;
use crate::spine::model::TOOL_RESPONSE_TRIM_THRESHOLD_BYTES;
use crate::spine::model::TOOL_RESULT_CLEARED_MESSAGE;
use crate::spine::model::TrimBodyUpdate;
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
use crate::spine::runtime::SpineTrimUpdateOutcome;
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
    ) -> Result<Vec<TrimBodyUpdate>, SpineError> {
        if !self.trim_enabled {
            return Ok(Vec::new());
        }
        if record_boundary {
            self.append_event(TrimEvent::ToolCallBoundary {
                toolcall_seq,
                raw_boundary: self.raw_len,
                raw_live_hash: hash_raw_live(self.raw_live),
            })?;
        }
        if raw_items.is_empty() {
            return Ok(Vec::new());
        }
        let projection_seq = toolcall_seq.checked_add(1).ok_or_else(|| {
            SpineError::InvalidEvent("spine trim toolcall seq overflow".to_string())
        })?;
        let existing_projection = self.current_projection(projection_seq)?;
        let mut updates = Vec::new();
        for segment in &toolcall.segments {
            if let Some(update) = self.append_candidate_for_response_segment(
                toolcall_seq,
                segment,
                raw_items,
                &existing_projection,
            )? {
                updates.push(update);
            }
        }
        Ok(updates)
    }

    pub(super) fn snip(
        &mut self,
        trim_id: &str,
        latest_live_completed_toolcall_seq: Option<u64>,
        current_structural_seq: u64,
    ) -> Result<SpineTrimUpdateOutcome, SpineError> {
        let trim_id = trim_id.trim().to_string();
        let Some(target) = self.target_in_latest_toolcall(
            &trim_id,
            latest_live_completed_toolcall_seq,
            current_structural_seq,
        )?
        else {
            return Ok(SpineTrimUpdateOutcome::without_updates(
                SpineTrimOutcome::Miss { trim_id },
            ));
        };
        if matches!(target.state, TrimTargetState::Snipped) {
            return Ok(SpineTrimUpdateOutcome::without_updates(
                SpineTrimOutcome::AlreadyCleared { trim_id },
            ));
        }
        self.append_event(TrimEvent::Snipped {
            trim_id: trim_id.clone(),
            raw_boundary: self.raw_len,
            raw_live_hash: hash_raw_live(self.raw_live),
        })?;
        let mut updated_target = target;
        updated_target.state = TrimTargetState::Snipped;
        Ok(SpineTrimUpdateOutcome::with_update(
            SpineTrimOutcome::Cleared { trim_id },
            TrimBodyUpdate::from_target(&updated_target, TOOL_RESULT_CLEARED_MESSAGE.to_string()),
        ))
    }

    pub(super) fn slice(
        &mut self,
        trim_id: &str,
        slice: TrimSliceSpec,
        latest_live_completed_toolcall_seq: Option<u64>,
        current_structural_seq: u64,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<SpineTrimUpdateOutcome, SpineError> {
        let trim_id = trim_id.trim().to_string();
        let Some(target) = self.target_in_latest_toolcall(
            &trim_id,
            latest_live_completed_toolcall_seq,
            current_structural_seq,
        )?
        else {
            return Ok(SpineTrimUpdateOutcome::without_updates(
                SpineTrimOutcome::Miss { trim_id },
            ));
        };
        let current_body = current_visible_body(&target, raw_items)?;
        let Some(visible_body) = apply_slice(&current_body, &slice) else {
            return Ok(SpineTrimUpdateOutcome::without_updates(
                SpineTrimOutcome::Miss { trim_id },
            ));
        };
        self.append_event(TrimEvent::Sliced {
            trim_id: trim_id.clone(),
            raw_boundary: self.raw_len,
            raw_live_hash: hash_raw_live(self.raw_live),
            slice,
            visible_body: visible_body.clone(),
        })?;
        let mut updated_target = target;
        updated_target.state = TrimTargetState::Sliced {
            visible_body: visible_body.clone(),
        };
        Ok(SpineTrimUpdateOutcome::with_update(
            SpineTrimOutcome::Sliced { trim_id },
            TrimBodyUpdate::from_target(&updated_target, visible_body),
        ))
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
        Ok(projection
            .targets_by_id
            .get(trim_id)
            .filter(|target| Some(target.toolcall_seq) == latest_live_completed_toolcall_seq)
            .cloned())
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

    fn append_candidate_for_response_segment(
        &mut self,
        toolcall_seq: u64,
        segment: &crate::spine::runtime::CompletedToolCallSegment,
        raw_items: &[Option<ResponseItem>],
        existing_projection: &TrimProjection,
    ) -> Result<Option<TrimBodyUpdate>, SpineError> {
        if segment.kind != crate::spine::model::ToolCallSegmentKind::Response
            || existing_projection.contains_toolcall_raw_target(toolcall_seq, segment.raw_ordinal)
        {
            return Ok(None);
        }
        let raw_index = trim_raw_ordinal_usize(segment.raw_ordinal)?;
        let Some(item) = raw_items.get(raw_index).and_then(Option::as_ref) else {
            return Ok(None);
        };
        let Some((response_kind, call_id)) = trimmable_text_tool_response(item) else {
            return Ok(None);
        };
        let original_visible_size = estimate_response_item_model_visible_bytes(item);
        if original_visible_size <= TOOL_RESPONSE_TRIM_THRESHOLD_BYTES {
            return Ok(None);
        }
        let trim_id = format!("trim_{}", *self.next_trim_seq);
        let target = TrimTarget {
            trim_id: trim_id.clone(),
            toolcall_seq,
            raw_ordinal: segment.raw_ordinal,
            call_id: call_id.to_string(),
            response_kind,
            original_visible_size,
            state: TrimTargetState::Tagged,
        };
        let visible_body = format!(
            "[TRIM_ID: {}]\n{}",
            target.trim_id,
            current_tagged_visible_body(&target, raw_items)?
        );
        self.append_event(TrimEvent::Candidate {
            trim_id,
            toolcall_seq,
            raw_ordinal: segment.raw_ordinal,
            call_id: call_id.to_string(),
            response_kind,
            original_visible_size,
        })?;
        Ok(Some(TrimBodyUpdate::from_target(&target, visible_body)))
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
                    call_id: call_id.clone(),
                    response_kind: *response_kind,
                    original_visible_size: *original_visible_size,
                    state: TrimTargetState::Tagged,
                });
            }
            TrimEvent::Cleared { trim_id, .. } | TrimEvent::Snipped { trim_id, .. } => {
                if let Some(target) = projection.targets_by_id.get_mut(trim_id) {
                    target.state = TrimTargetState::Snipped;
                }
            }
            TrimEvent::Sliced {
                trim_id,
                visible_body,
                ..
            } => {
                if let Some(target) = projection.targets_by_id.get_mut(trim_id) {
                    target.state = TrimTargetState::Sliced {
                        visible_body: visible_body.clone(),
                    };
                }
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
        TrimTargetState::Tagged => current_tagged_visible_body(target, raw_items),
        TrimTargetState::Sliced { visible_body } => Ok(visible_body.clone()),
        TrimTargetState::Snipped => Ok(TOOL_RESULT_CLEARED_MESSAGE.to_string()),
    }
}

fn current_tagged_visible_body(
    target: &TrimTarget,
    raw_items: &[Option<ResponseItem>],
) -> Result<String, SpineError> {
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
    let tag = format!("[TRIM_ID: {}]\n", target.trim_id);
    Ok(text.strip_prefix(&tag).unwrap_or(&text).to_string())
}

fn trim_raw_ordinal_usize(raw_ordinal: u64) -> Result<usize, SpineError> {
    usize::try_from(raw_ordinal)
        .map_err(|_| SpineError::InvalidEvent("trim raw ordinal overflow".to_string()))
}

fn apply_slice(text: &str, slice: &TrimSliceSpec) -> Option<String> {
    match slice {
        TrimSliceSpec::Head { head } => Some(text[..prefix_byte_index(text, *head)].to_string()),
        TrimSliceSpec::Tail { tail } => {
            let keep_from = text.chars().count().saturating_sub(*tail);
            Some(text[prefix_byte_index(text, keep_from)..].to_string())
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
            let prefix = &text[..anchor_start];
            let start =
                prefix_byte_index(prefix, prefix.chars().count().saturating_sub(*preceding));
            let suffix = &text[anchor_end..];
            let end = anchor_end + prefix_byte_index(suffix, *following);
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
