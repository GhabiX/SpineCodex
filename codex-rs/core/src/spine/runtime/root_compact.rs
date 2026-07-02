use codex_protocol::models::ResponseItem;
use std::path::Path;

use super::SpineError;
#[cfg(test)]
use super::SpineRootCompactResult;
use super::SpineRootCompactTokenMetadata;
use super::SpineRuntime;
use super::prepared::SpinePreparedRootCompact;
use crate::spine::archive::memory_ref;
use crate::spine::io::hash_raw_live;
use crate::spine::io::sha1_hex;
use crate::spine::model::MemKind;
use crate::spine::model::MemRecord;
use crate::spine::model::MemoryRef;
use crate::spine::model::NodeId;
use crate::spine::model::SpineLedgerEvent;
use crate::spine::store::BODY_DIR;

struct RootCompactMemoryArtifact {
    mem: MemRecord,
    memory: MemoryRef,
    raw_live_hash: String,
}

struct PreparedRootCompactCommit {
    prepared_root_compact: SpinePreparedRootCompact,
    mem: MemRecord,
    memory_body: String,
    compact_checkpoint: Option<crate::spine::compact_checkpoint::SpineCompactCheckpoint>,
    root_compact_event: SpineLedgerEvent,
}

pub(crate) fn spine_root_compact_body(replaced_context: &[ResponseItem]) -> Option<String> {
    if replaced_context.is_empty() {
        return None;
    }
    serde_json::to_string_pretty(replaced_context).ok()
}

fn root_epoch_rendered_context_item_count(body: &str) -> Option<usize> {
    serde_json::from_str::<Vec<ResponseItem>>(body)
        .ok()
        .and_then(|items| (!items.is_empty()).then_some(items.len()))
}

impl SpineRuntime {
    #[cfg(test)]
    pub(crate) fn root_compact(
        &mut self,
        body: String,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<Vec<ResponseItem>, SpineError> {
        let prepared = self.root_compact_impl(
            body,
            raw_items,
            SpineRootCompactTokenMetadata::default(),
            None,
        )?;
        let publication = self.install_prepared_root_compact_for_direct_publication(prepared);
        Ok(publication.variable_context().to_vec())
    }

    #[cfg(test)]
    pub(crate) fn root_compact_with_checkpoint(
        &mut self,
        rollout_path: &Path,
        body: String,
        raw_items: &[Option<ResponseItem>],
        token_metadata: SpineRootCompactTokenMetadata,
    ) -> Result<SpineRootCompactResult, SpineError> {
        let prepared = self.prepare_root_compact_with_checkpoint(
            rollout_path,
            body,
            raw_items,
            token_metadata,
        )?;
        Ok(self.install_prepared_root_compact_for_direct_publication(prepared))
    }

    pub(crate) fn prepare_root_compact_with_checkpoint(
        &mut self,
        rollout_path: &Path,
        body: String,
        raw_items: &[Option<ResponseItem>],
        token_metadata: SpineRootCompactTokenMetadata,
    ) -> Result<SpinePreparedRootCompact, SpineError> {
        self.root_compact_impl(body, raw_items, token_metadata, Some(rollout_path))
    }

    fn root_compact_impl(
        &mut self,
        body: String,
        raw_items: &[Option<ResponseItem>],
        token_metadata: SpineRootCompactTokenMetadata,
        checkpoint_rollout_path: Option<&Path>,
    ) -> Result<SpinePreparedRootCompact, SpineError> {
        let token_metadata = SpineRootCompactTokenMetadata {
            next_open_input_tokens: None,
            next_open_context_tokens: None,
            ..token_metadata
        };
        let prepared = self.prepare_root_compact_commit(
            body,
            raw_items,
            token_metadata,
            checkpoint_rollout_path,
        )?;
        if let Err(err) = self.commit_root_compact_prepared_side_effects(
            &prepared.mem,
            &prepared.memory_body,
            prepared.compact_checkpoint.as_ref(),
        ) {
            self.parser
                .install_pending_root_compact_after_side_effect_failure(
                    prepared
                        .prepared_root_compact
                        .parser_install_for_side_effect_failure(),
                );
            return Err(err);
        }
        let marker =
            super::support::root_compact_commit_marker(self.ledger.next_event_seq, &prepared.mem)?;
        self.append_committed_events(vec![prepared.root_compact_event], marker)?;
        self.pending = None;
        Ok(prepared.prepared_root_compact)
    }

    pub(crate) fn install_prepared_root_compact(&mut self, prepared: SpinePreparedRootCompact) {
        prepared.consume_parser_install(|parser_install| {
            self.parser.install_prepared_root_compact(parser_install);
        });
    }

    #[cfg(test)]
    fn install_prepared_root_compact_for_direct_publication(
        &mut self,
        prepared: SpinePreparedRootCompact,
    ) -> SpineRootCompactResult {
        prepared.consume_for_direct_publication(|parser_install| {
            self.parser.install_prepared_root_compact(parser_install);
        })
    }

    fn commit_root_compact_prepared_side_effects(
        &mut self,
        mem: &MemRecord,
        memory_body: &str,
        compact_checkpoint: Option<&crate::spine::compact_checkpoint::SpineCompactCheckpoint>,
    ) -> Result<(), SpineError> {
        self.write_prepared_memory_body(mem, memory_body)
            .and_then(|()| self.commit_prepared_memory_record(mem, memory_body))
            .and_then(|()| {
                if let Some(checkpoint) = compact_checkpoint {
                    self.store.append_compact_checkpoint(checkpoint)?;
                }
                Ok(())
            })
    }

    fn prepare_root_compact_commit(
        &self,
        body: String,
        raw_items: &[Option<ResponseItem>],
        token_metadata: SpineRootCompactTokenMetadata,
        checkpoint_rollout_path: Option<&Path>,
    ) -> Result<PreparedRootCompactCommit, SpineError> {
        if body.trim().is_empty() {
            return Err(SpineError::CompactFailure(
                "spine root compact memory body must not be empty".to_string(),
            ));
        }
        let trim_projection = self.current_trim_projection()?;
        let source_context_end = self
            .parser
            .variable_context_len(raw_items, &trim_projection)?;
        let node = self.parser.current_root_epoch_id()?;
        let seq = self.ledger.next_event_seq;
        let root_memory = self.build_root_compact_memory_artifact(
            &node,
            &body,
            source_context_end,
            token_metadata,
            seq,
        );

        let staged_memory_body = Some((root_memory.mem.compact_id.as_str(), body.as_str()));
        let next_open_index_usize = self.parser.root_compact_next_open_index_or_probe(
            &root_memory.memory,
            token_metadata.next_open_input_tokens,
            token_metadata.next_open_context_tokens,
            raw_items,
            staged_memory_body,
            &trim_projection,
            &self.archive(),
        )?;

        let prepared_txn = self.parser.prepare_root_compact_txn(
            root_memory.memory.clone(),
            next_open_index_usize,
            token_metadata.next_open_input_tokens,
            token_metadata.next_open_context_tokens,
            raw_items,
            staged_memory_body,
            &trim_projection,
            &self.archive(),
        )?;
        prepared_txn.validate_current_open_matches_variable_context_len()?;
        let token_seq_after = seq.checked_add(1).ok_or_else(|| {
            SpineError::InvalidEvent("root compact token seq overflow".to_string())
        })?;
        let compact_checkpoint = checkpoint_rollout_path
            .map(|rollout_path| {
                prepared_txn.build_compact_checkpoint(
                    rollout_path,
                    self.raw_len,
                    token_seq_after,
                    &self.raw_live,
                    raw_items,
                )
            })
            .transpose()?;
        let root_compact_event = crate::spine::lexer::plan_root_compact().lex_event(
            node,
            self.raw_len,
            root_memory.memory,
            next_open_index_usize,
            root_memory.raw_live_hash,
            token_metadata.next_open_input_tokens,
            token_metadata.next_open_context_tokens,
        )?;
        Ok(PreparedRootCompactCommit {
            prepared_root_compact: SpinePreparedRootCompact::from_parser_prepared_txn(
                self.raw_len,
                token_seq_after,
                prepared_txn,
            ),
            mem: root_memory.mem,
            memory_body: body,
            compact_checkpoint,
            root_compact_event,
        })
    }

    fn build_root_compact_memory_artifact(
        &self,
        node: &NodeId,
        body: &str,
        source_context_end: usize,
        token_metadata: SpineRootCompactTokenMetadata,
        root_event_seq: u64,
    ) -> RootCompactMemoryArtifact {
        let compact_id = format!("root-{}-{}", node.as_path().replace('.', "-"), self.raw_len);
        let raw_live_hash = hash_raw_live(&self.raw_live);
        let mem = MemRecord {
            compact_id: compact_id.clone(),
            kind: MemKind::RootEpoch,
            node: node.clone(),
            raw_start: 0,
            raw_end: self.raw_len,
            context_start: 0,
            context_end: source_context_end,
            rendered_context_item_count: root_epoch_rendered_context_item_count(body),
            raw_live_hash: Some(raw_live_hash.clone()),
            open_input_tokens: None,
            close_input_tokens: token_metadata.close_input_tokens,
            open_context_tokens: None,
            close_context_tokens: token_metadata.close_context_tokens,
            closed_source_suffix_tokens: None,
            closed_memory_context_tokens: None,
            open_context_source: None,
            memory_output_tokens: None,
            body_path: format!("{BODY_DIR}/{compact_id}.md"),
            body_hash: sha1_hex(body.as_bytes()),
        };
        let memory = memory_ref(
            &self.archive(),
            mem.compact_id.clone(),
            mem.node.clone(),
            mem.body_hash.clone(),
            mem.raw_start..mem.raw_end,
            mem.context_start..mem.context_end,
            root_event_seq..root_event_seq + 1,
            mem.raw_live_hash.clone(),
            mem.rendered_context_item_count,
            mem.open_input_tokens,
            mem.close_input_tokens,
            mem.open_context_tokens,
            mem.close_context_tokens,
            mem.closed_source_suffix_tokens,
            mem.closed_memory_context_tokens,
            mem.open_context_source,
            mem.memory_output_tokens,
        );
        RootCompactMemoryArtifact {
            mem,
            memory,
            raw_live_hash,
        }
    }
}
