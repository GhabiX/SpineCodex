use super::sidecar_store_path;
use crate::spine::SpineError;
use crate::spine::checkpoint::CheckpointMemoryRef;
use crate::spine::compact_checkpoint::SpineCompactCheckpoint;
use crate::spine::io::sha1_hex;
use crate::spine::model::LoggedSpineLedgerEvent;
use crate::spine::model::MemKind;
use crate::spine::model::MemRecord;
use crate::spine::model::SpineLedgerEvent;
use std::collections::BTreeSet;
use std::path::Path;

pub(super) fn validate_compact_checkpoint_root_marker(
    store_root: &Path,
    checkpoint: &SpineCompactCheckpoint,
    events: &[LoggedSpineLedgerEvent],
    mems: &[MemRecord],
) -> Result<(), SpineError> {
    let Some(root_event_seq) = checkpoint.token_seq.checked_sub(1) else {
        return Err(SpineError::InvalidStore(format!(
            "spine compact checkpoint at raw boundary {} has no root compact token predecessor",
            checkpoint.raw_boundary
        )));
    };
    let mut matching_events = events.iter().filter(|event| event.seq == root_event_seq);
    let Some(root_event) = matching_events.next() else {
        return Err(SpineError::InvalidStore(format!(
            "missing RootCompact ledger marker for compact checkpoint at raw boundary {} token_seq {}",
            checkpoint.raw_boundary, checkpoint.token_seq
        )));
    };
    if matching_events.next().is_some() {
        return Err(SpineError::InvalidStore(format!(
            "ambiguous RootCompact ledger marker for compact checkpoint at raw boundary {} token_seq {}",
            checkpoint.raw_boundary, checkpoint.token_seq
        )));
    }
    let SpineLedgerEvent::RootCompact {
        node,
        boundary,
        mem,
        raw_live_hash,
        ..
    } = &root_event.event
    else {
        return Err(SpineError::InvalidStore(format!(
            "compact checkpoint at raw boundary {} token_seq {} is not preceded by RootCompact",
            checkpoint.raw_boundary, checkpoint.token_seq
        )));
    };
    if *boundary != checkpoint.raw_boundary {
        return Err(SpineError::InvalidStore(format!(
            "RootCompact boundary {} does not match compact checkpoint raw boundary {}",
            boundary, checkpoint.raw_boundary
        )));
    }
    if raw_live_hash != &checkpoint.raw_live_hash {
        return Err(SpineError::InvalidStore(format!(
            "RootCompact raw live hash mismatch at compact checkpoint raw boundary {}",
            checkpoint.raw_boundary
        )));
    }
    let mem_record = unique_root_compact_memory(mem, mems)?;
    if !matches!(mem_record.kind, MemKind::RootEpoch) {
        return Err(SpineError::InvalidStore(format!(
            "RootCompact ledger marker references non-root memory {mem}"
        )));
    }
    if &mem_record.node != node {
        return Err(SpineError::InvalidStore(format!(
            "RootCompact node {node} does not match memory node {}",
            mem_record.node
        )));
    }
    if mem_record.raw_end != checkpoint.raw_boundary
        || mem_record.raw_live_hash.as_deref() != Some(checkpoint.raw_live_hash.as_str())
    {
        return Err(SpineError::InvalidStore(format!(
            "RootCompact memory {} does not match compact checkpoint boundary {}",
            mem_record.compact_id, checkpoint.raw_boundary
        )));
    }
    let mut matching_memory_refs = checkpoint
        .memory_refs
        .iter()
        .filter(|memory| memory.compact_id == *mem);
    let Some(memory_ref) = matching_memory_refs.next() else {
        return Err(SpineError::InvalidStore(format!(
            "compact checkpoint at raw boundary {} is missing RootCompact memory ref {mem}",
            checkpoint.raw_boundary
        )));
    };
    if matching_memory_refs.next().is_some() {
        return Err(SpineError::InvalidStore(format!(
            "compact checkpoint at raw boundary {} has ambiguous RootCompact memory ref {mem}",
            checkpoint.raw_boundary
        )));
    }
    validate_checkpoint_memory_ref_for_mem(
        store_root,
        checkpoint,
        memory_ref,
        mem_record,
        root_event.seq..checkpoint.token_seq,
    )
}

pub(super) fn validate_compact_checkpoint_memory_refs(
    store_root: &Path,
    checkpoint: &SpineCompactCheckpoint,
    mems: &[MemRecord],
) -> Result<(), SpineError> {
    let mut compact_ids = BTreeSet::new();
    for memory in &checkpoint.memory_refs {
        if !compact_ids.insert(memory.compact_id.clone()) {
            return Err(SpineError::InvalidStore(format!(
                "duplicate compact checkpoint memory ref {} at raw boundary {}",
                memory.compact_id, checkpoint.raw_boundary
            )));
        }
        let mem_record = unique_checkpoint_memory(checkpoint, memory, mems)?;
        validate_checkpoint_memory_ref_for_committed_mem(
            store_root, checkpoint, memory, mem_record,
        )?;
    }
    Ok(())
}

fn unique_root_compact_memory<'a>(
    compact_id: &str,
    mems: &'a [MemRecord],
) -> Result<&'a MemRecord, SpineError> {
    let mut matching_mems = mems.iter().filter(|record| record.compact_id == compact_id);
    let Some(mem_record) = matching_mems.next() else {
        return Err(SpineError::InvalidStore(format!(
            "RootCompact ledger marker references missing memory {compact_id}"
        )));
    };
    if matching_mems.next().is_some() {
        return Err(SpineError::InvalidStore(format!(
            "RootCompact ledger marker references ambiguous memory {compact_id}"
        )));
    }
    Ok(mem_record)
}

fn unique_checkpoint_memory<'a>(
    checkpoint: &SpineCompactCheckpoint,
    memory: &CheckpointMemoryRef,
    mems: &'a [MemRecord],
) -> Result<&'a MemRecord, SpineError> {
    let mut matching_mems = mems
        .iter()
        .filter(|record| record.compact_id == memory.compact_id);
    let Some(mem_record) = matching_mems.next() else {
        return Err(SpineError::InvalidStore(format!(
            "compact checkpoint memory ref {} references missing committed memory at raw boundary {}",
            memory.compact_id, checkpoint.raw_boundary
        )));
    };
    if matching_mems.next().is_some() {
        return Err(SpineError::InvalidStore(format!(
            "compact checkpoint memory ref {} references ambiguous committed memory at raw boundary {}",
            memory.compact_id, checkpoint.raw_boundary
        )));
    }
    Ok(mem_record)
}

fn validate_checkpoint_memory_ref_for_mem(
    store_root: &Path,
    checkpoint: &SpineCompactCheckpoint,
    memory: &CheckpointMemoryRef,
    mem: &MemRecord,
    token_seq: std::ops::Range<u64>,
) -> Result<(), SpineError> {
    validate_checkpoint_memory_ref_for_committed_mem(store_root, checkpoint, memory, mem)?;
    if memory.source_token_seq_start != token_seq.start
        || memory.source_token_seq_end != token_seq.end
    {
        return Err(SpineError::InvalidStore(format!(
            "compact checkpoint RootCompact memory ref {} does not match committed memory record at raw boundary {}",
            memory.compact_id, checkpoint.raw_boundary
        )));
    }
    Ok(())
}

fn validate_checkpoint_memory_ref_for_committed_mem(
    store_root: &Path,
    checkpoint: &SpineCompactCheckpoint,
    memory: &CheckpointMemoryRef,
    mem: &MemRecord,
) -> Result<(), SpineError> {
    let mem_body_path = sidecar_store_path(store_root, &mem.body_path);
    let checkpoint_body_path = sidecar_store_path(store_root, &memory.body_path);
    if memory.node_id != mem.node.to_string()
        || memory.body_hash != mem.body_hash
        || memory.source_raw_start != mem.raw_start
        || memory.source_raw_end != mem.raw_end
        || memory.source_context_start != mem.context_start
        || memory.source_context_end != mem.context_end
        || memory.open_input_tokens != mem.open_input_tokens
        || memory.close_input_tokens != mem.close_input_tokens
        || memory.open_context_tokens != mem.open_context_tokens
        || memory.close_context_tokens != mem.close_context_tokens
        || memory.closed_source_suffix_tokens != mem.closed_source_suffix_tokens
        || memory.closed_memory_context_tokens != mem.closed_memory_context_tokens
        || memory.open_context_source != mem.open_context_source
        || memory.memory_output_tokens != mem.memory_output_tokens
        || checkpoint_body_path != mem_body_path
    {
        return Err(SpineError::InvalidStore(format!(
            "compact checkpoint memory ref {} does not match committed memory record at raw boundary {}",
            memory.compact_id, checkpoint.raw_boundary
        )));
    }
    let body = std::fs::read_to_string(checkpoint_body_path)?;
    if sha1_hex(body.as_bytes()) != memory.body_hash {
        return Err(SpineError::InvalidStore(format!(
            "memory body hash mismatch for {}",
            memory.compact_id
        )));
    }
    Ok(())
}
