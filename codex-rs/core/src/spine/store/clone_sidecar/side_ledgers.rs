use super::super::SpineCloneBoundary;
use super::super::SpineStore;
use crate::spine::SpineError;
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
        if boundary
            .pressure_seq_watermark
            .is_some_and(|watermark| pressure.pressure_seq <= watermark)
            && pressure.allowed_by(source_raw_live)
        {
            target.append_logged_pressure_event(&pressure)?;
        }
    }
    for trim in source_trim_events {
        if boundary
            .trim_seq_watermark
            .is_some_and(|watermark| trim.trim_seq <= watermark)
            && trim.allowed_by(mask)?
            && trim
                .event
                .within_toolcall_boundary(boundary.trim_toolcall_seq_limit)
        {
            target.append_logged_trim_event(&trim)?;
        }
    }
    Ok(())
}
