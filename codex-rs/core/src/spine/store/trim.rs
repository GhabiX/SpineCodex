use crate::spine::SpineError;
use crate::spine::io::create_parent_dir;
use crate::spine::model::LoggedTrimEvent;
use crate::spine::model::TrimEvent;
use std::fs::OpenOptions;
use std::path::Path;

pub(super) fn ensure_ledger_exists(path: &Path) -> Result<(), SpineError> {
    create_parent_dir(path)?;
    OpenOptions::new().create(true).append(true).open(path)?;
    Ok(())
}

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
    events
        .iter()
        .filter(|event| trim_event_within_raw_boundary(&event.event, raw_boundary))
        .map(|event| event.trim_seq)
        .max()
}

fn trim_event_within_raw_boundary(event: &TrimEvent, raw_boundary: u64) -> bool {
    match event {
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
    }
}

pub(super) fn toolcall_seq_limit_from_events(
    events: &[LoggedTrimEvent],
    trim_seq_watermark: Option<u64>,
) -> Result<u64, SpineError> {
    events
        .iter()
        .filter(|event| trim_seq_watermark.is_none_or(|watermark| event.trim_seq <= watermark))
        .filter_map(|event| match &event.event {
            TrimEvent::ToolCallBoundary { toolcall_seq, .. } => Some(*toolcall_seq),
            _ => None,
        })
        .max()
        .map_or(Ok(0), |toolcall_seq| {
            toolcall_seq.checked_add(1).ok_or_else(|| {
                SpineError::InvalidEvent("spine trim toolcall seq overflow".to_string())
            })
        })
}
