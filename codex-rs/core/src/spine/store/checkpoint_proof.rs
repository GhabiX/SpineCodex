use super::memory_body;
use super::sidecar_store_path;
use crate::spine::SpineError;
use crate::spine::checkpoint::CheckpointMemoryRef;
use crate::spine::compact_checkpoint::SpineCompactCheckpoint;
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
    let root_event = unique_root_compact_event(checkpoint, events)?;
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
    let memory_ref = unique_root_compact_checkpoint_memory_ref(checkpoint, mem)?;
    validate_checkpoint_memory_ref(
        store_root,
        checkpoint,
        memory_ref,
        mem_record,
        Some(root_event.seq..checkpoint.token_seq),
    )
}

fn unique_root_compact_event<'a>(
    checkpoint: &SpineCompactCheckpoint,
    events: &'a [LoggedSpineLedgerEvent],
) -> Result<&'a LoggedSpineLedgerEvent, SpineError> {
    let Some(root_event_seq) = checkpoint.token_seq.checked_sub(1) else {
        return Err(SpineError::InvalidStore(format!(
            "spine compact checkpoint at raw boundary {} has no root compact token predecessor",
            checkpoint.raw_boundary
        )));
    };
    unique_one(
        events.iter().filter(|event| event.seq == root_event_seq),
        SpineError::InvalidStore(format!(
            "missing RootCompact ledger marker for compact checkpoint at raw boundary {} token_seq {}",
            checkpoint.raw_boundary, checkpoint.token_seq
        )),
        SpineError::InvalidStore(format!(
            "ambiguous RootCompact ledger marker for compact checkpoint at raw boundary {} token_seq {}",
            checkpoint.raw_boundary, checkpoint.token_seq
        )),
    )
}

fn unique_root_compact_checkpoint_memory_ref<'a>(
    checkpoint: &'a SpineCompactCheckpoint,
    compact_id: &str,
) -> Result<&'a CheckpointMemoryRef, SpineError> {
    unique_one(
        checkpoint
            .memory_refs
            .iter()
            .filter(|memory| memory.compact_id == compact_id),
        SpineError::InvalidStore(format!(
            "compact checkpoint at raw boundary {} is missing RootCompact memory ref {compact_id}",
            checkpoint.raw_boundary
        )),
        SpineError::InvalidStore(format!(
            "compact checkpoint at raw boundary {} has ambiguous RootCompact memory ref {compact_id}",
            checkpoint.raw_boundary
        )),
    )
}

pub(super) fn validate_compact_checkpoint_memory_refs(
    store_root: &Path,
    checkpoint: &SpineCompactCheckpoint,
    mems: &[MemRecord],
) -> Result<(), SpineError> {
    let mut compact_ids = BTreeSet::new();
    for memory in &checkpoint.memory_refs {
        if !compact_ids.insert(memory.compact_id.as_str()) {
            return Err(SpineError::InvalidStore(format!(
                "duplicate compact checkpoint memory ref {} at raw boundary {}",
                memory.compact_id, checkpoint.raw_boundary
            )));
        }
        let mem_record = unique_checkpoint_memory(checkpoint, memory, mems)?;
        validate_checkpoint_memory_ref(store_root, checkpoint, memory, mem_record, None)?;
    }
    Ok(())
}

fn unique_root_compact_memory<'a>(
    compact_id: &str,
    mems: &'a [MemRecord],
) -> Result<&'a MemRecord, SpineError> {
    unique_memory_by_compact_id(
        compact_id,
        mems,
        || format!("RootCompact ledger marker references missing memory {compact_id}"),
        || format!("RootCompact ledger marker references ambiguous memory {compact_id}"),
    )
}

fn unique_checkpoint_memory<'a>(
    checkpoint: &SpineCompactCheckpoint,
    memory: &CheckpointMemoryRef,
    mems: &'a [MemRecord],
) -> Result<&'a MemRecord, SpineError> {
    unique_memory_by_compact_id(
        &memory.compact_id,
        mems,
        || {
            format!(
                "compact checkpoint memory ref {} references missing committed memory at raw boundary {}",
                memory.compact_id, checkpoint.raw_boundary
            )
        },
        || {
            format!(
                "compact checkpoint memory ref {} references ambiguous committed memory at raw boundary {}",
                memory.compact_id, checkpoint.raw_boundary
            )
        },
    )
}

fn unique_memory_by_compact_id<'a>(
    compact_id: &str,
    mems: &'a [MemRecord],
    missing_message: impl FnOnce() -> String,
    ambiguous_message: impl FnOnce() -> String,
) -> Result<&'a MemRecord, SpineError> {
    unique_one(
        mems.iter().filter(|record| record.compact_id == compact_id),
        SpineError::InvalidStore(missing_message()),
        SpineError::InvalidStore(ambiguous_message()),
    )
}

fn unique_one<T>(
    mut items: impl Iterator<Item = T>,
    missing_error: SpineError,
    ambiguous_error: SpineError,
) -> Result<T, SpineError> {
    let Some(item) = items.next() else {
        return Err(missing_error);
    };
    if items.next().is_some() {
        return Err(ambiguous_error);
    }
    Ok(item)
}

fn validate_checkpoint_memory_ref(
    store_root: &Path,
    checkpoint: &SpineCompactCheckpoint,
    memory: &CheckpointMemoryRef,
    mem: &MemRecord,
    token_seq: Option<std::ops::Range<u64>>,
) -> Result<(), SpineError> {
    let mem_body_path = sidecar_store_path(store_root, &mem.body_path);
    let checkpoint_body_path = sidecar_store_path(store_root, &memory.body_path);
    if !checkpoint_memory_ref_matches_record(memory, mem, &checkpoint_body_path, &mem_body_path) {
        return Err(SpineError::InvalidStore(format!(
            "compact checkpoint memory ref {} does not match committed memory record at raw boundary {}",
            memory.compact_id, checkpoint.raw_boundary
        )));
    }
    memory_body::read_body_with_hash(checkpoint_body_path, &memory.compact_id, &memory.body_hash)?;
    if let Some(token_seq) = token_seq {
        if memory.source_token_seq_start != token_seq.start
            || memory.source_token_seq_end != token_seq.end
        {
            return Err(SpineError::InvalidStore(format!(
                "compact checkpoint RootCompact memory ref {} does not match committed memory record at raw boundary {}",
                memory.compact_id, checkpoint.raw_boundary
            )));
        }
    }
    Ok(())
}

fn checkpoint_memory_ref_matches_record(
    memory: &CheckpointMemoryRef,
    mem: &MemRecord,
    checkpoint_body_path: &Path,
    mem_body_path: &Path,
) -> bool {
    memory.node_id == mem.node.to_string()
        && memory.body_hash == mem.body_hash
        && memory.source_raw_start == mem.raw_start
        && memory.source_raw_end == mem.raw_end
        && memory.source_context_start == mem.context_start
        && memory.source_context_end == mem.context_end
        && memory.open_input_tokens == mem.open_input_tokens
        && memory.close_input_tokens == mem.close_input_tokens
        && memory.open_context_tokens == mem.open_context_tokens
        && memory.close_context_tokens == mem.close_context_tokens
        && memory.closed_source_suffix_tokens == mem.closed_source_suffix_tokens
        && memory.closed_memory_context_tokens == mem.closed_memory_context_tokens
        && memory.open_context_source == mem.open_context_source
        && memory.memory_output_tokens == mem.memory_output_tokens
        && checkpoint_body_path == mem_body_path
}
