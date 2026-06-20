use crate::spine::SpineError;
use crate::spine::model::LoggedTrimEvent;
use crate::spine::model::TrimEvent;

pub(super) fn event_within_toolcall_boundary(
    event: &LoggedTrimEvent,
    toolcall_seq_limit: u64,
) -> bool {
    match &event.event {
        TrimEvent::ToolCallBoundary { toolcall_seq, .. }
        | TrimEvent::Candidate { toolcall_seq, .. } => *toolcall_seq < toolcall_seq_limit,
        TrimEvent::Cleared { .. } | TrimEvent::Snipped { .. } | TrimEvent::Sliced { .. } => true,
    }
}

pub(super) fn seq_watermark_for_raw_boundary(
    events: &[LoggedTrimEvent],
    raw_boundary: u64,
) -> Option<u64> {
    let mut watermark = None;
    for event in events {
        let within_boundary = match &event.event {
            TrimEvent::ToolCallBoundary {
                raw_boundary: event_boundary,
                ..
            }
            | TrimEvent::Cleared {
                raw_boundary: event_boundary,
                ..
            }
            | TrimEvent::Snipped {
                raw_boundary: event_boundary,
                ..
            }
            | TrimEvent::Sliced {
                raw_boundary: event_boundary,
                ..
            } => *event_boundary <= raw_boundary,
            TrimEvent::Candidate { raw_ordinal, .. } => *raw_ordinal < raw_boundary,
        };
        if within_boundary {
            watermark =
                Some(watermark.map_or(event.trim_seq, |current: u64| current.max(event.trim_seq)));
        }
    }
    watermark
}

pub(super) fn toolcall_seq_limit_from_events(
    events: &[LoggedTrimEvent],
    trim_seq_watermark: Option<u64>,
) -> Result<u64, SpineError> {
    Ok(events
        .iter()
        .filter(|event| trim_seq_watermark.is_none_or(|watermark| event.trim_seq <= watermark))
        .filter_map(|event| match &event.event {
            TrimEvent::ToolCallBoundary { toolcall_seq, .. } => Some(*toolcall_seq),
            _ => None,
        })
        .max()
        .map(|toolcall_seq| {
            toolcall_seq.checked_add(1).ok_or_else(|| {
                SpineError::InvalidEvent("spine trim toolcall seq overflow".to_string())
            })
        })
        .transpose()?
        .unwrap_or(0))
}
