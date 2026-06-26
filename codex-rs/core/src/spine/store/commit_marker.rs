use crate::spine::SpineError;
use crate::spine::model::COMMIT_MARKER_VERSION;
use crate::spine::model::SpineCommitMarker;

mod event_shape;
mod memory_refs;
mod replay;

pub(super) use event_shape::validate_commit_marker_events;
pub(super) use memory_refs::commit_marker_allowed_by_source_live;
pub(super) use replay::validate_markers_for_replay;

pub(super) fn validate_commit_marker_record(marker: &SpineCommitMarker) -> Result<(), SpineError> {
    if marker.version != COMMIT_MARKER_VERSION {
        return Err(SpineError::InvalidStore(format!(
            "unsupported Spine commit marker version {}",
            marker.version
        )));
    }
    if marker.op_id.trim().is_empty() {
        return Err(SpineError::InvalidStore(
            "Spine commit marker op_id must not be empty".to_string(),
        ));
    }
    if marker.token_seq_start >= marker.token_seq_end {
        return Err(SpineError::InvalidStore(format!(
            "invalid Spine commit marker token range {}..{}",
            marker.token_seq_start, marker.token_seq_end
        )));
    }
    if marker.memory_refs.is_empty() {
        return Err(SpineError::InvalidStore(format!(
            "Spine commit marker {} must reference memory artifacts",
            marker.op_id
        )));
    }
    Ok(())
}
