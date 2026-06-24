use crate::spine::SpineError;
use crate::spine::io::create_parent_dir;
use crate::spine::model::LoggedTrimEvent;
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
    event.event.within_toolcall_boundary(toolcall_seq_limit)
}

pub(super) fn seq_watermark_for_raw_boundary(
    events: &[LoggedTrimEvent],
    raw_boundary: u64,
) -> Option<u64> {
    events
        .iter()
        .filter(|event| event.event.within_raw_boundary(raw_boundary))
        .map(|event| event.trim_seq)
        .max()
}

pub(super) fn toolcall_seq_limit_from_events(
    events: &[LoggedTrimEvent],
    trim_seq_watermark: Option<u64>,
) -> Result<u64, SpineError> {
    events
        .iter()
        .filter(|event| trim_seq_watermark.is_none_or(|watermark| event.trim_seq <= watermark))
        .filter_map(|event| event.event.toolcall_boundary_seq())
        .max()
        .map_or(Ok(0), |toolcall_seq| {
            toolcall_seq.checked_add(1).ok_or_else(|| {
                SpineError::InvalidEvent("spine trim toolcall seq overflow".to_string())
            })
        })
}
