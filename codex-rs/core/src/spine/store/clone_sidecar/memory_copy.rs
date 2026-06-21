use super::super::SpineStore;
use crate::spine::SpineError;
use crate::spine::model::MemRecord;
use crate::spine::model::RawMask;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

pub(in crate::spine::store::clone_sidecar) fn copy_required_memories(
    source: &SpineStore,
    target: &SpineStore,
    source_mems: Vec<MemRecord>,
    required_memory_ids: &BTreeSet<String>,
    mask: RawMask<'_>,
) -> Result<BTreeMap<String, String>, SpineError> {
    let mut cloned_memory_paths = BTreeMap::new();
    for mem in source_mems {
        if mem.allowed_by(mask)? {
            // Memory records do not carry a structural sequence, so any
            // raw-visible record must still be readable. Only records
            // referenced by cloned events/checkpoints are copied.
            let body = source.read_memory_body(&mem)?;
            if required_memory_ids.contains(&mem.compact_id) {
                let body_path = target.write_memory_body(&mem.compact_id, &body)?;
                cloned_memory_paths.insert(mem.compact_id.clone(), body_path.clone());
                let cloned = MemRecord { body_path, ..mem };
                target.append_mem(&cloned)?;
            }
        }
    }
    for accounting in source.mem_accounting()? {
        if cloned_memory_paths.contains_key(&accounting.compact_id) {
            target.append_mem_accounting(&accounting)?;
        }
    }
    for witness in source.mem_accounting_witnesses()? {
        if cloned_memory_paths.contains_key(witness.compact_id()) {
            target.append_mem_accounting_witness(&witness)?;
        }
    }
    Ok(cloned_memory_paths)
}
