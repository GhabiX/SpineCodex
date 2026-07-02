use super::SpineError;
use super::SpineRuntime;
use super::types::SpineCloseMemoryAssembly;
use super::types::SpineTokenBaselines;
use crate::spine::archive::SpineArchive;
use crate::spine::archive::memory_ref;
use crate::spine::io::hash_raw_live;
use crate::spine::io::sha1_hex;
use crate::spine::model::MemKind;
use crate::spine::model::MemRecord;
use crate::spine::model::MemoryRef;
use crate::spine::model::NodeId;
use crate::spine::model::SpineLedgerEvent;
use crate::spine::model::TreeMeta;
use crate::spine::store::BODY_DIR;
use std::path::PathBuf;

pub(super) fn memory_ref_for_committed_mem(
    archive: &SpineArchive,
    mem: &MemRecord,
    event_seq: u64,
) -> MemoryRef {
    memory_ref(
        archive,
        mem.compact_id.clone(),
        mem.node.clone(),
        mem.body_hash.clone(),
        mem.raw_start..mem.raw_end,
        mem.context_start..mem.context_end,
        event_seq..event_seq + 1,
        mem.raw_live_hash.clone(),
        mem.open_input_tokens,
        mem.close_input_tokens,
        mem.open_context_tokens,
        mem.close_context_tokens,
        mem.closed_source_suffix_tokens,
        mem.closed_memory_context_tokens,
        mem.open_context_source,
        mem.memory_output_tokens,
    )
}

impl SpineRuntime {
    pub(super) fn write_prepared_memory_body(
        &self,
        mem: &MemRecord,
        body: &str,
    ) -> Result<(), SpineError> {
        self.store
            .write_memory_body(&mem.compact_id, body)
            .map(|_| ())
    }

    pub(super) fn prepared_memory_body_path(&self, mem: &MemRecord) -> PathBuf {
        self.store.memory_body_path(mem)
    }

    pub(super) fn commit_prepared_memory_record(
        &self,
        mem: &MemRecord,
        body: &str,
    ) -> Result<(), SpineError> {
        let existing_mems = self.store.mems()?;
        let mut matching_mems = existing_mems
            .iter()
            .filter(|existing| existing.compact_id == mem.compact_id);
        match (matching_mems.next(), matching_mems.next()) {
            (None, _) => self.store.append_mem(mem),
            (Some(existing), None) if existing == mem => {
                self.validate_existing_prepared_memory_body(existing, mem, body)
            }
            (Some(_), None) => Err(SpineError::InvalidStore(format!(
                "existing prepared memory record mismatch for {}",
                mem.compact_id
            ))),
            (Some(_), Some(_)) => Err(SpineError::InvalidStore(format!(
                "ambiguous existing prepared memory record for {}",
                mem.compact_id
            ))),
        }
    }

    fn validate_existing_prepared_memory_body(
        &self,
        existing: &MemRecord,
        mem: &MemRecord,
        body: &str,
    ) -> Result<(), SpineError> {
        let existing_body = self.store.read_memory_body(existing)?;
        let existing_body_hash = sha1_hex(existing_body.as_bytes());
        let body_hash = sha1_hex(body.as_bytes());
        if existing_body_hash != body_hash || existing_body_hash != mem.body_hash {
            return Err(SpineError::InvalidStore(format!(
                "existing prepared memory body mismatch for {}",
                mem.compact_id
            )));
        }
        Ok(())
    }

    pub(super) fn stage_close_mem(
        &self,
        open_meta: &TreeMeta,
        memory_assembly: &SpineCloseMemoryAssembly,
        token_baselines: SpineTokenBaselines,
    ) -> Result<MemRecord, SpineError> {
        let node_id = open_meta.id.clone();
        let raw_start = memory_assembly.source_raw_range.start;
        let end = memory_assembly.source_raw_range.end;
        let raw_end = usize::try_from(end)
            .map_err(|_| SpineError::InvalidEvent("memory raw end overflow".to_string()))?;
        let raw_live_prefix = self.raw_live.get(..raw_end).ok_or_else(|| {
            SpineError::InvalidEvent(format!(
                "memory raw end {} exceeds raw live length {}",
                end,
                self.raw_live.len()
            ))
        })?;
        let compact_id = format!(
            "mem-{}-{}-{}",
            node_id.as_path().replace('.', "-"),
            raw_start,
            end
        );
        let body_path = format!("{BODY_DIR}/{compact_id}.md");
        let open_context_baseline =
            self.open_context_baseline_for(open_meta)
                .map_err(|problem| {
                    SpineError::InvalidEvent(format!(
                        "corrupt provider input baseline for node {}: {problem:?}",
                        open_meta.id
                    ))
                })?;
        let open_input_tokens = open_meta.open_input_tokens;
        let open_context_tokens =
            open_context_baseline.map(|baseline| baseline.provider_input_tokens);
        let closed_source_suffix_tokens = open_context_baseline
            .map(|baseline| baseline.provider_input_tokens)
            .zip(token_baselines.provider_input_tokens)
            .and_then(|(open, close)| (close >= open).then_some(close - open));
        let mem = MemRecord {
            compact_id,
            kind: MemKind::Suffix,
            node: node_id,
            raw_start,
            raw_end: end,
            context_start: memory_assembly.source_context_range.start,
            context_end: memory_assembly.source_context_range.end,
            raw_live_hash: Some(hash_raw_live(raw_live_prefix)),
            open_input_tokens,
            close_input_tokens: token_baselines.provider_input_tokens,
            open_context_tokens,
            close_context_tokens: token_baselines.provider_input_tokens,
            closed_source_suffix_tokens,
            closed_memory_context_tokens: None,
            open_context_source: open_context_baseline.map(|baseline| baseline.source),
            memory_output_tokens: memory_assembly.memory_output_tokens,
            body_path,
            body_hash: sha1_hex(memory_assembly.body.as_bytes()),
        };
        Ok(mem)
    }

    pub(super) fn open_raw_start(&self, node_id: &NodeId) -> Result<u64, SpineError> {
        let events = &self.ledger.events;
        if let Some(boundary) = events.iter().rev().find_map(|event| match &event.event {
            SpineLedgerEvent::Open {
                child, boundary, ..
            } if child == node_id => Some(*boundary),
            _ => None,
        }) {
            return Ok(boundary);
        }
        let Some(parent) = node_id.parent() else {
            return Err(SpineError::SidecarCorruption(format!(
                "missing open event for {node_id}; node has no parent"
            )));
        };
        if node_id.is_first_root_epoch_child() {
            return self.open_raw_start_from_root_compact(node_id, &parent);
        }
        Err(SpineError::SidecarCorruption(format!(
            "missing open event for {node_id}; no matching open/root compact event in sidecar"
        )))
    }

    fn open_raw_start_from_root_compact(
        &self,
        node_id: &NodeId,
        parent: &NodeId,
    ) -> Result<u64, SpineError> {
        let root_epoch = parent
            .0
            .first()
            .copied()
            .ok_or_else(|| SpineError::InvalidEvent("root epoch id is empty".to_string()))?;
        let Some(previous_root_epoch) = root_epoch.checked_sub(1) else {
            return Err(SpineError::SidecarCorruption(format!(
                "missing open event for {node_id}; root epoch {root_epoch} has no previous compact boundary"
            )));
        };
        let compacted_parent = NodeId::root_epoch(previous_root_epoch);
        self.ledger
            .events
            .iter()
            .rev()
            .find_map(|event| match &event.event {
                SpineLedgerEvent::RootCompact { node, boundary, .. }
                    if *node == compacted_parent && parent.child(1) == *node_id =>
                {
                    Some(*boundary)
                }
                _ => None,
            })
            .ok_or_else(|| {
                SpineError::SidecarCorruption(format!(
                    "missing open event for {node_id}; no root compact boundary for parent {parent}"
                ))
            })
    }
}
