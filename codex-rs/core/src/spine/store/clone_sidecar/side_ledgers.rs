use super::super::SpineCloneBoundary;
use super::super::SpineStore;
use super::super::trim;
use crate::spine::SpineError;
use crate::spine::model::LoggedPressureEvent;
use crate::spine::model::LoggedTrimEvent;
use crate::spine::model::RawMask;

pub(in crate::spine::store::clone_sidecar) fn copy_pressure_and_trim(
    source: &SpineStore,
    target: &SpineStore,
    source_trim_events: Vec<LoggedTrimEvent>,
    boundary: &SpineCloneBoundary,
    source_raw_live: &[bool],
    mask: RawMask<'_>,
) -> Result<(), SpineError> {
    for pressure in source.pressure_events()? {
        if pressure_event_in_clone_boundary(&pressure, boundary, source_raw_live) {
            target.append_logged_pressure_event(&pressure)?;
        }
    }
    for trim in source_trim_events {
        if trim_event_in_clone_boundary(&trim, boundary, mask)? {
            target.append_logged_trim_event(&trim)?;
        }
    }
    Ok(())
}

fn pressure_event_in_clone_boundary(
    pressure: &LoggedPressureEvent,
    boundary: &SpineCloneBoundary,
    source_raw_live: &[bool],
) -> bool {
    boundary
        .pressure_seq_watermark
        .is_some_and(|watermark| pressure.pressure_seq <= watermark)
        && pressure.allowed_by(source_raw_live)
}

fn trim_event_in_clone_boundary(
    event: &LoggedTrimEvent,
    boundary: &SpineCloneBoundary,
    mask: RawMask<'_>,
) -> Result<bool, SpineError> {
    Ok(boundary
        .trim_seq_watermark
        .is_some_and(|watermark| event.trim_seq <= watermark)
        && event.allowed_by(mask)?
        && trim::event_within_toolcall_boundary(event, boundary.trim_toolcall_seq_limit))
}
