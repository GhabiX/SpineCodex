use super::SpineError;
use super::SpineRuntime;
use super::support::mem_record_matches;
use super::types::SpineCloseMemoryAssembly;
use super::types::SpineTokenBaselines;
use crate::spine::io::sha1_hex;
use crate::spine::model::MemKind;
use crate::spine::model::MemRecord;
use crate::spine::model::NodeId;
use crate::spine::model::SpineLedgerEvent;
use crate::spine::model::TreeMeta;
use crate::spine::store::BODY_DIR;

impl SpineRuntime {
    pub(super) fn write_prepared_memory_body(
        &self,
        mem: &MemRecord,
        body: &str,
    ) -> Result<(), SpineError> {
        self.store.write_memory_body(&mem.compact_id, body)?;
        Ok(())
    }

    pub(super) fn commit_prepared_memory_record(
        &self,
        mem: &MemRecord,
        body: &str,
    ) -> Result<(), SpineError> {
        let existing_mems = self.store.mems()?;
        let matching_mems = existing_mems
            .iter()
            .filter(|existing| existing.compact_id == mem.compact_id)
            .collect::<Vec<_>>();
        match matching_mems.as_slice() {
            [] => self.store.append_mem(mem),
            [existing] if mem_record_matches(existing, mem) => {
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
            [_] => Err(SpineError::InvalidStore(format!(
                "existing prepared memory record mismatch for {}",
                mem.compact_id
            ))),
            _ => Err(SpineError::InvalidStore(format!(
                "ambiguous existing prepared memory record for {}",
                mem.compact_id
            ))),
        }
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
            raw_live_hash: None,
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
        if parent.is_root_epoch() && node_id.0.last() == Some(&1) {
            let root_epoch =
                parent.0.first().copied().ok_or_else(|| {
                    SpineError::InvalidEvent("root epoch id is empty".to_string())
                })?;
            let Some(previous_root_epoch) = root_epoch.checked_sub(1) else {
                return Err(SpineError::SidecarCorruption(format!(
                    "missing open event for {node_id}; root epoch {root_epoch} has no previous compact boundary"
                )));
            };
            let compacted_parent = NodeId::root_epoch(previous_root_epoch);
            return events
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
                });
        }
        Err(SpineError::SidecarCorruption(format!(
            "missing open event for {node_id}; no matching open/root compact event in sidecar"
        )))
    }
}
