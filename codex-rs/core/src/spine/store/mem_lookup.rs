use crate::spine::SpineError;
use crate::spine::model::MemRecord;

pub(super) fn unique_mem_record_by_compact_id<'a>(
    compact_id: &str,
    mems: &'a [MemRecord],
    missing_message: impl FnOnce() -> String,
    ambiguous_message: impl FnOnce() -> String,
) -> Result<&'a MemRecord, SpineError> {
    let mut matching_mems = mems.iter().filter(|record| record.compact_id == compact_id);
    let Some(mem) = matching_mems.next() else {
        return Err(SpineError::InvalidStore(missing_message()));
    };
    if matching_mems.next().is_some() {
        return Err(SpineError::InvalidStore(ambiguous_message()));
    }
    Ok(mem)
}
