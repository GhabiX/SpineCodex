use codex_protocol::models::ResponseItem;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;

use super::SpineError;
use super::SpineLedgerCache;
use super::SpineRuntime;
use super::accounting::pending_memory_context_accounting_from_store;
use super::replay::next_user_anchor_from_events;
use super::replay::replay_event_seqs_from_markers;
use super::replay::replay_from_events;
use crate::spine::archive::SpineArchive;
use crate::spine::checkpoint::SpineCheckpoint;
use crate::spine::checkpoint::validate_checkpoint;
use crate::spine::model::NodeId;
use crate::spine::model::RawMask;
use crate::spine::model::SpineLedgerEvent;
use crate::spine::parse_stack::ParseStack;
use crate::spine::parse_stack::parse_stack_from_events_with_forced_events;
use crate::spine::store::SpineStore;

impl SpineRuntime {
    pub(crate) fn load_or_create(rollout_path: &Path, raw_len: u64) -> Result<Self, SpineError> {
        Self::load_or_create_with_jit(rollout_path, raw_len, true)
    }

    pub(crate) fn load_or_create_with_jit(
        rollout_path: &Path,
        raw_len: u64,
        jit_enabled: bool,
    ) -> Result<Self, SpineError> {
        let store = SpineStore::load_or_create_for_writer(rollout_path)?;
        if !jit_enabled {
            let raw_len_usize = usize::try_from(raw_len)
                .map_err(|_| SpineError::InvalidEvent("raw item count overflow".to_string()))?;
            return Self::load_trim_only(store, vec![true; raw_len_usize]);
        }
        if jit_enabled && !store.tree_path().exists() {
            store.append_event(&SpineLedgerEvent::Init { raw_start: 0 })?;
            store.append_event(&SpineLedgerEvent::Open {
                child: NodeId::root_epoch(1).child(1),
                boundary: raw_len,
                index: raw_len,
                summary: "root".to_string(),
                open_input_tokens: None,
                open_context_tokens: None,
                open_context_source: None,
            })?;
        }
        let mut runtime = Self::load(store, raw_len)?;
        runtime.set_jit_enabled(jit_enabled);
        Ok(runtime)
    }

    fn load_trim_only(store: SpineStore, raw_live: Vec<bool>) -> Result<Self, SpineError> {
        let ledger = SpineLedgerCache::new(Vec::new(), Vec::new(), store.trim_events()?)?;
        let next_user_anchor = next_user_anchor_from_events(&ledger.events)?;
        Ok(Self {
            store,
            ledger,
            parse_stack: ParseStack::new(),
            raw_len: u64::try_from(raw_live.len())
                .map_err(|_| SpineError::InvalidEvent("raw item count overflow".to_string()))?,
            raw_live,
            jit_enabled: false,
            trim_enabled: true,
            open_requests: BTreeMap::new(),
            control_call_ids: BTreeSet::new(),
            tree_call_ids: BTreeSet::new(),
            ordinary_tool_requests: BTreeMap::new(),
            #[cfg(test)]
            pending_tool_responses: BTreeMap::new(),
            pending: None,
            pending_memory_context_accounting: None,
            next_user_anchor,
        })
    }

    pub(crate) fn load_for_rollout_items(
        rollout_path: &Path,
        raw_items: &[Option<ResponseItem>],
        rollback_cuts: &[usize],
    ) -> Result<Option<Self>, SpineError> {
        if !SpineStore::has_for_rollout(rollout_path)? {
            return Ok(None);
        }
        let runtime = Self::load_for_rollout_items_from_store(
            SpineStore::for_rollout(rollout_path)?,
            rollout_path,
            raw_items,
            rollback_cuts,
        )?;
        Ok(Some(runtime))
    }

    pub(crate) fn load_for_rollout_items_for_writer(
        rollout_path: &Path,
        raw_items: &[Option<ResponseItem>],
        rollback_cuts: &[usize],
    ) -> Result<Option<Self>, SpineError> {
        Self::load_for_rollout_items_for_writer_with_jit(
            rollout_path,
            raw_items,
            rollback_cuts,
            true,
        )
    }

    pub(crate) fn load_for_rollout_items_for_writer_with_jit(
        rollout_path: &Path,
        raw_items: &[Option<ResponseItem>],
        rollback_cuts: &[usize],
        jit_enabled: bool,
    ) -> Result<Option<Self>, SpineError> {
        if !SpineStore::has_for_rollout(rollout_path)? {
            return Ok(None);
        }
        if !jit_enabled {
            if !rollback_cuts.is_empty() {
                return Err(SpineError::InvalidStore(
                    "spine_trim-only replay does not support rollback cuts".to_string(),
                ));
            }
            let raw_live = raw_items.iter().map(Option::is_some).collect();
            let runtime = Self::load_trim_only(
                SpineStore::for_rollout(rollout_path)?.with_writer_lock()?,
                raw_live,
            )?;
            return Ok(Some(runtime));
        }
        let runtime = Self::load_for_rollout_items_from_store(
            SpineStore::for_rollout(rollout_path)?.with_writer_lock()?,
            rollout_path,
            raw_items,
            rollback_cuts,
        )?;
        Ok(Some(runtime))
    }

    fn load_for_rollout_items_from_store(
        store: SpineStore,
        rollout_path: &Path,
        raw_items: &[Option<ResponseItem>],
        rollback_cuts: &[usize],
    ) -> Result<Self, SpineError> {
        let runtime = Self::load_with_raw_live_for_rollout(
            store,
            raw_items.iter().map(Option::is_some).collect(),
            rollback_cuts,
            rollout_path,
            raw_items,
        )?;
        runtime.validate_raw_coverage(raw_items)?;
        Ok(runtime)
    }

    #[cfg(test)]
    pub(crate) fn load_for_rollout(
        rollout_path: &Path,
        raw_len: u64,
    ) -> Result<Option<Self>, SpineError> {
        if !SpineStore::has_for_rollout(rollout_path)? {
            return Ok(None);
        }
        Self::load(SpineStore::for_rollout(rollout_path)?, raw_len).map(Some)
    }

    pub(crate) fn load(store: SpineStore, raw_len: u64) -> Result<Self, SpineError> {
        let raw_len_usize = usize::try_from(raw_len)
            .map_err(|_| SpineError::InvalidEvent("raw item count overflow".to_string()))?;
        Self::load_with_raw_live(store, vec![true; raw_len_usize])
    }

    pub(crate) fn acquire_writer_lock(&mut self) -> Result<(), SpineError> {
        self.store.ensure_writer_lock()
    }

    fn load_with_raw_live(store: SpineStore, raw_live: Vec<bool>) -> Result<Self, SpineError> {
        Self::load_with_raw_live_and_event_limit(store, raw_live, None)
    }

    fn load_with_raw_live_for_rollout(
        store: SpineStore,
        raw_live: Vec<bool>,
        rollback_cuts: &[usize],
        rollout_path: &Path,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<Self, SpineError> {
        let checkpoint = store.rollback_checkpoint(rollback_cuts)?;
        let trim_events = store.trim_events()?;
        if let Some(checkpoint) = checkpoint.as_ref() {
            validate_checkpoint(checkpoint, rollout_path, &raw_live, raw_items, &trim_events)?;
            return Self::load_with_rollback_checkpoint(store, raw_live, checkpoint);
        }
        if let Some(checkpoint) = store.resume_checkpoint(raw_live.len())? {
            validate_checkpoint(
                &checkpoint,
                rollout_path,
                &raw_live,
                raw_items,
                &trim_events,
            )?;
            Self::validate_checkpoint_parse_stack_prefix(&store, &raw_live, &checkpoint)?;
        }
        Self::load_with_raw_live(store, raw_live)
    }

    fn validate_checkpoint_parse_stack_prefix(
        store: &SpineStore,
        raw_live: &[bool],
        checkpoint: &SpineCheckpoint,
    ) -> Result<(), SpineError> {
        let ledger = SpineLedgerCache::new(
            store.events()?,
            store.pressure_events()?,
            store.trim_events()?,
        )?;
        let mems = store.mems()?;
        let markers = store.commit_markers()?;
        store.validate_commit_markers_for_replay(
            &ledger.events,
            &mems,
            raw_live,
            None,
            Some(checkpoint.token_seq),
        )?;
        let archive = SpineArchive::new(store.root.clone());
        let raw_ordinal = usize::try_from(checkpoint.raw_ordinal)
            .map_err(|_| SpineError::InvalidEvent("checkpoint raw ordinal overflow".to_string()))?;
        let prefix_live = &raw_live[..raw_ordinal.min(raw_live.len())];
        let prefix_mask = RawMask::new(prefix_live);
        let prefix_events = ledger
            .events
            .iter()
            .filter(|event| event.seq < checkpoint.token_seq)
            .cloned()
            .collect::<Vec<_>>();
        let prefix_replay_event_seqs = replay_event_seqs_from_markers(
            &ledger.events,
            &markers,
            &mems,
            prefix_mask,
            None,
            Some(checkpoint.token_seq),
            true,
        )?;
        let prefix_ps = parse_stack_from_events_with_forced_events(
            &prefix_events,
            &archive,
            &mems,
            prefix_mask,
            &prefix_replay_event_seqs.forced,
            &prefix_replay_event_seqs.marker_structural,
        )?;
        if prefix_ps != checkpoint.parse_stack {
            return Err(SpineError::Invariant(format!(
                "spine checkpoint ParseStack mismatch for {} at raw_ordinal={} token_seq={}",
                checkpoint.checkpoint_id, checkpoint.raw_ordinal, checkpoint.token_seq
            )));
        }
        Ok(())
    }

    pub(super) fn load_with_raw_live_and_event_limit(
        store: SpineStore,
        raw_live: Vec<bool>,
        event_limit: Option<u64>,
    ) -> Result<Self, SpineError> {
        let ledger = SpineLedgerCache::new(
            store.events()?,
            store.pressure_events()?,
            store.trim_events()?,
        )?;
        let next_user_anchor = next_user_anchor_from_events(&ledger.events)?;
        let mems = store.mems()?;
        let markers = store.commit_markers()?;
        store.validate_commit_markers_for_replay(
            &ledger.events,
            &mems,
            &raw_live,
            None,
            event_limit,
        )?;
        let replay_event_seqs = replay_event_seqs_from_markers(
            &ledger.events,
            &markers,
            &mems,
            RawMask::new(&raw_live),
            None,
            event_limit,
            true,
        )?;
        let events = if let Some(limit) = event_limit {
            ledger
                .events
                .iter()
                .filter(|event| event.seq < limit)
                .cloned()
                .collect::<Vec<_>>()
        } else {
            ledger.events.clone()
        };
        let archive = SpineArchive::new(store.root.clone());
        let parse_stack = replay_from_events(
            &archive,
            &events,
            &mems,
            &raw_live,
            &replay_event_seqs,
            None,
            None,
        )?;
        let pending_memory_context_accounting =
            pending_memory_context_accounting_from_store(&store)?;
        Ok(Self {
            store,
            ledger,
            parse_stack,
            raw_len: u64::try_from(raw_live.len())
                .map_err(|_| SpineError::InvalidEvent("raw item count overflow".to_string()))?,
            raw_live,
            jit_enabled: true,
            trim_enabled: true,
            open_requests: BTreeMap::new(),
            control_call_ids: BTreeSet::new(),
            tree_call_ids: BTreeSet::new(),
            ordinary_tool_requests: BTreeMap::new(),
            #[cfg(test)]
            pending_tool_responses: BTreeMap::new(),
            pending: None,
            pending_memory_context_accounting,
            next_user_anchor,
        })
    }

    fn load_with_rollback_checkpoint(
        store: SpineStore,
        raw_live: Vec<bool>,
        checkpoint: &SpineCheckpoint,
    ) -> Result<Self, SpineError> {
        let mut ledger = SpineLedgerCache::new(
            store.events()?,
            store.pressure_events()?,
            store.trim_events()?,
        )?;
        let next_user_anchor = next_user_anchor_from_events(&ledger.events)?;
        ledger.retain_trim_events_at_or_before(checkpoint.trim_seq_watermark);
        let mems = store.mems()?;
        let markers = store.commit_markers()?;
        store.validate_commit_markers_for_replay(
            &ledger.events,
            &mems,
            &raw_live,
            Some(checkpoint.token_seq),
            None,
        )?;
        let replay_event_seqs = replay_event_seqs_from_markers(
            &ledger.events,
            &markers,
            &mems,
            RawMask::new(&raw_live),
            Some(checkpoint.token_seq),
            None,
            false,
        )?;
        let archive = SpineArchive::new(store.root.clone());
        let raw_ordinal = usize::try_from(checkpoint.raw_ordinal)
            .map_err(|_| SpineError::InvalidEvent("checkpoint raw ordinal overflow".to_string()))?;
        let prefix_live = &raw_live[..raw_ordinal.min(raw_live.len())];
        let prefix_mask = RawMask::new(prefix_live);
        let prefix_events = ledger
            .events
            .iter()
            .filter(|event| event.seq < checkpoint.token_seq)
            .cloned()
            .collect::<Vec<_>>();
        let prefix_replay_event_seqs = replay_event_seqs_from_markers(
            &ledger.events,
            &markers,
            &mems,
            prefix_mask,
            None,
            Some(checkpoint.token_seq),
            true,
        )?;
        let prefix_ps = parse_stack_from_events_with_forced_events(
            &prefix_events,
            &archive,
            &mems,
            prefix_mask,
            &prefix_replay_event_seqs.forced,
            &prefix_replay_event_seqs.marker_structural,
        )?;
        if prefix_ps != checkpoint.parse_stack {
            return Err(SpineError::Invariant(format!(
                "spine checkpoint ParseStack mismatch for {} at raw_ordinal={} token_seq={}",
                checkpoint.checkpoint_id, checkpoint.raw_ordinal, checkpoint.token_seq
            )));
        }

        let parse_stack = replay_from_events(
            &archive,
            &ledger.events,
            &mems,
            &raw_live,
            &replay_event_seqs,
            Some(&checkpoint.parse_stack),
            Some(checkpoint.token_seq),
        )?;
        let pending_memory_context_accounting =
            pending_memory_context_accounting_from_store(&store)?;
        Ok(Self {
            store,
            ledger,
            parse_stack,
            raw_len: u64::try_from(raw_live.len())
                .map_err(|_| SpineError::InvalidEvent("raw item count overflow".to_string()))?,
            raw_live,
            jit_enabled: true,
            trim_enabled: true,
            open_requests: BTreeMap::new(),
            control_call_ids: BTreeSet::new(),
            tree_call_ids: BTreeSet::new(),
            ordinary_tool_requests: BTreeMap::new(),
            #[cfg(test)]
            pending_tool_responses: BTreeMap::new(),
            pending: None,
            pending_memory_context_accounting,
            next_user_anchor,
        })
    }
}
