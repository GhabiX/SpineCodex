use super::fast_fail::RuntimeFastFailError;
use super::fast_fail::mem_install_body_error;
use super::fast_fail::validate_mem_install_metadata;
use super::fast_fail::validate_mem_install_pre_commit;
use super::ids::NodeId;
use super::ids::NodeIdParseError;
use super::mem_install::GENERATED_MEMORY_SECTION_MARKER;
use super::mem_install::GeneratedMemorySection;
use super::mem_install::MemoryBodyError;
use super::mem_install::MemoryBodyRef;
use super::mem_install::MemorySectionId;
use super::mem_install::parse_generated_memory_sections;
use super::mem_install::verify_memory_body_ref as verify_memory_body_ref_in_memory;
use super::projection_epoch::ProjectionEpochMetadata;
use super::state::SpineOperationName;
use super::state::SpineState;
use super::state::SpineStateError;
use super::state::StateCheckpoint;
use super::state::StateCheckpointError;
use super::state::Transition;
use codex_protocol::protocol::RolloutItem;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Write;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::MutexGuard;
use thiserror::Error;

const TREE_FILE: &str = "tree.jsonl";
const NODES_DIR: &str = "nodes";
const MEMORY_FILE: &str = "memory.md";
const NODE_TRAJS_FILE: &str = "trajs.jsonl";
const TRAJS_INDEX_FILE: &str = "trajs.index.jsonl";
const COMPACT_INDEX_FILE: &str = "compact.index.jsonl";
const RAW_DIR: &str = "raw";
const RAW_ROLLOUT_FILE: &str = "rollout.raw.jsonl";
const SPINE_BASE_LOCATOR_VERSION: u32 = 1;
const MEM_INSTALL_COMMITTED_SCHEMA_VERSION: u32 = 3;

#[derive(Clone, Debug)]
pub(crate) struct SpineSidecarStore {
    root: PathBuf,
    metadata_cache: Arc<Mutex<SpineStoreMetadataCache>>,
}

impl PartialEq for SpineSidecarStore {
    fn eq(&self, other: &Self) -> bool {
        self.root == other.root
    }
}

impl Eq for SpineSidecarStore {}

#[derive(Debug, Default)]
struct SpineStoreMetadataCache {
    // The JSONL ledger remains authoritative. This cache is replay-derived and advances only after
    // a tree event append succeeds.
    next_tree_seq: Option<u64>,
}

impl SpineSidecarStore {
    pub(crate) fn for_rollout(rollout_path: impl AsRef<Path>) -> Result<Self, SpineStoreError> {
        let rollout_path = rollout_path.as_ref();
        let locator_path = Self::locator_path_for_rollout(rollout_path)?;
        let locator = read_base_locator(&locator_path)?;
        let parent = rollout_parent(rollout_path)?;
        let base = PathBuf::from(locator.base);
        validate_relative_base(&base, rollout_path)?;
        Ok(Self::new(parent.join(base)))
    }

    pub(crate) fn create_for_rollout(
        rollout_path: impl AsRef<Path>,
    ) -> Result<Self, SpineStoreError> {
        let rollout_path = rollout_path.as_ref();
        let locator_path = Self::locator_path_for_rollout(rollout_path)?;
        if locator_path.exists() {
            return Err(SpineStoreError::AlreadyInitialized { path: locator_path });
        }
        let root = Self::default_sidecar_dir_for_rollout(rollout_path)?;
        if root.exists() {
            return Err(SpineStoreError::AlreadyInitialized { path: root });
        }
        Self::write_base_locator(rollout_path, &locator_path, &root)?;
        Ok(Self::new(root))
    }

    fn new(root: PathBuf) -> Self {
        Self {
            root,
            metadata_cache: Arc::new(Mutex::new(SpineStoreMetadataCache::default())),
        }
    }

    pub(crate) fn has_sidecar_for_rollout(rollout_path: &Path) -> Result<bool, SpineStoreError> {
        Ok(Self::locator_path_for_rollout(rollout_path)?.exists())
    }

    fn write_base_locator(
        rollout_path: &Path,
        locator_path: &Path,
        root: &Path,
    ) -> Result<(), SpineStoreError> {
        let base = root
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or_else(|| SpineStoreError::InvalidRolloutPath {
                path: rollout_path.to_path_buf(),
                reason: "rollout path must produce a valid UTF-8 spine base",
            })?
            .to_string();
        let locator = SpineBaseLocator {
            version: SPINE_BASE_LOCATOR_VERSION,
            base,
        };
        let contents =
            serde_json::to_string_pretty(&locator).map_err(|source| SpineStoreError::Json {
                path: locator_path.to_path_buf(),
                source,
            })? + "\n";
        if let Some(parent) = locator_path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| SpineStoreError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        std::fs::write(locator_path, contents).map_err(|source| SpineStoreError::Io {
            path: locator_path.to_path_buf(),
            source,
        })
    }

    pub(crate) fn locator_path_for_rollout(
        rollout_path: &Path,
    ) -> Result<PathBuf, SpineStoreError> {
        let parent = rollout_parent(rollout_path)?;
        let stem = rollout_stem(rollout_path)?;
        Ok(parent.join(format!("{stem}.spine.json")))
    }

    pub(crate) fn default_sidecar_dir_for_rollout(
        rollout_path: &Path,
    ) -> Result<PathBuf, SpineStoreError> {
        let parent = rollout_parent(rollout_path)?;
        let stem = rollout_stem(rollout_path)?;
        Ok(parent.join(format!("spine-{stem}")))
    }

    fn validate_rollout_path(rollout_path: &Path) -> Result<(), SpineStoreError> {
        if rollout_path
            .extension()
            .and_then(|extension| extension.to_str())
            != Some("jsonl")
        {
            return Err(SpineStoreError::InvalidRolloutPath {
                path: rollout_path.to_path_buf(),
                reason: "rollout path must use the .jsonl extension",
            });
        }
        Ok(())
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn tree_path(&self) -> PathBuf {
        self.root.join(TREE_FILE)
    }

    pub(crate) fn trajs_index_path(&self) -> PathBuf {
        self.root.join(TRAJS_INDEX_FILE)
    }

    pub(crate) fn compact_index_path(&self) -> PathBuf {
        self.root.join(COMPACT_INDEX_FILE)
    }

    pub(crate) fn raw_rollout_path(&self) -> PathBuf {
        self.root.join(RAW_DIR).join(RAW_ROLLOUT_FILE)
    }

    pub(crate) fn node_dir(&self, node_id: &NodeId) -> PathBuf {
        let mut path = self.root.join(NODES_DIR);
        for segment in node_id.segments() {
            path.push(segment.to_string());
        }
        path
    }

    pub(crate) fn memory_path(&self, node_id: &NodeId) -> PathBuf {
        self.node_dir(node_id).join(MEMORY_FILE)
    }

    pub(crate) fn node_trajs_path(&self, node_id: &NodeId) -> PathBuf {
        self.node_dir(node_id).join(NODE_TRAJS_FILE)
    }

    pub(crate) fn create(&self) -> Result<SpineState, SpineStoreError> {
        let tree_path = self.tree_path();
        if tree_path.exists() {
            return Err(SpineStoreError::AlreadyInitialized {
                path: self.root.clone(),
            });
        }

        self.ensure_sidecar_dir()?;
        self.ensure_node_dir(&NodeId::root_epoch(1))?;
        self.ensure_node_dir(&NodeId::root_epoch(1).child(1))?;
        self.create_trajs_index_file()?;
        self.create_compact_index_file()?;
        self.create_raw_rollout_file()?;

        let initial_raw_start_ordinal = 0;
        let state = SpineState::new_with_initial_leaf_raw_start(initial_raw_start_ordinal);
        let event = TreeEvent::SpineInitialized {
            seq: 1,
            initial_raw_start_ordinal,
        };
        self.append_json_line(&tree_path, &event)?;
        self.set_cached_next_tree_seq(2)?;

        Ok(state)
    }

    pub(crate) fn load(&self) -> Result<SpineState, SpineStoreError> {
        let state = self.replay_tree()?;
        self.validate_compact_index()?;
        Ok(state)
    }

    pub(crate) fn record_transition(
        &self,
        state: &mut SpineState,
        op: SpineOperation,
        summary: impl TransitionSummaryArg,
        raw_start_ordinal: u64,
        source_turn_id: impl Into<String>,
    ) -> Result<Transition, SpineStoreError> {
        let summary = summary.into_transition_summary();
        let source_turn_id = source_turn_id.into();
        let mut next_state = state.clone();
        let transition = op.apply(&mut next_state, summary.clone())?;
        next_state.set_raw_start_ordinal(&transition.to, raw_start_ordinal)?;
        self.ensure_node_dir(&transition.to)?;

        let event = TreeEvent::TransitionApplied {
            seq: self.next_tree_seq()?,
            op,
            from_node: transition.from.to_string(),
            to_node: transition.to.to_string(),
            summary,
            raw_start_ordinal,
            source_turn_id,
        };
        self.append_tree_event(&event)?;

        *state = next_state;
        Ok(transition)
    }

    pub(crate) fn record_root_epoch_archive(
        &self,
        state: &mut SpineState,
        summary: impl Into<String>,
        raw_start_ordinal: u64,
        compact_id: impl Into<String>,
        source_turn_id: impl Into<String>,
    ) -> Result<Transition, SpineStoreError> {
        let summary = summary.into();
        let compact_id = compact_id.into();
        let source_turn_id = source_turn_id.into();
        let mut next_state = state.clone();
        let transition = next_state.reset_root_epoch(summary.clone(), raw_start_ordinal)?;
        self.ensure_node_dir(&transition.to)?;

        let event = TreeEvent::RootEpochReset {
            seq: self.next_tree_seq()?,
            root_id: transition.from.to_string(),
            next_leaf_id: transition.to.to_string(),
            summary,
            raw_start_ordinal,
            compact_id,
            source_turn_id,
        };
        self.append_tree_event(&event)?;

        *state = next_state;
        Ok(transition)
    }

    pub(crate) fn record_raw_start_ordinal(
        &self,
        state: &mut SpineState,
        node_id: &NodeId,
        raw_start_ordinal: u64,
        source_turn_id: impl Into<String>,
    ) -> Result<(), SpineStoreError> {
        let mut next_state = state.clone();
        next_state.set_raw_start_ordinal(node_id, raw_start_ordinal)?;
        let event = TreeEvent::RawStartOrdinalUpdated {
            seq: self.next_tree_seq()?,
            node_id: node_id.to_string(),
            raw_start_ordinal,
            source_turn_id: source_turn_id.into(),
        };
        self.append_tree_event(&event)?;
        *state = next_state;
        Ok(())
    }

    pub(crate) fn record_projection_reset(
        &self,
        state: &SpineState,
        checkpoint: StateCheckpoint,
        reason: impl Into<String>,
        source_turn_id: Option<String>,
        epoch: ProjectionEpochMetadata,
    ) -> Result<(), SpineStoreError> {
        let replayed = checkpoint.replay()?;
        if &replayed != state {
            return Err(SpineStoreError::InvalidLedger(
                "projection reset checkpoint does not replay to projected spine state".to_string(),
            ));
        }
        for node_id in state.nodes().keys() {
            self.ensure_node_dir(node_id)?;
        }
        let event = TreeEvent::ProjectionReset {
            seq: self.next_tree_seq()?,
            reason: reason.into(),
            source_turn_id,
            source_rollout_ref: epoch.source_rollout_ref,
            processed_rollout_len: epoch.processed_rollout_len,
            processed_rollout_hash: epoch.processed_rollout_hash,
            effective_raw_len: epoch.effective_raw_len,
            surviving_turn_ids_hash: epoch.surviving_turn_ids_hash,
            surviving_compact_ids: epoch.surviving_compact_ids,
            checkpoint_hash: epoch.checkpoint_hash,
            checkpoint,
        };
        self.append_tree_event(&event)?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn copy_node_artifacts_from<'a>(
        &self,
        source: &SpineSidecarStore,
        node_ids: impl IntoIterator<Item = &'a NodeId>,
    ) -> Result<(), SpineStoreError> {
        for node_id in node_ids {
            self.ensure_node_dir(node_id)?;
            self.copy_node_file_if_present(source, node_id, MEMORY_FILE)?;
        }
        Ok(())
    }

    pub(crate) fn copy_projected_node_artifacts_from<'a>(
        &self,
        source: &SpineSidecarStore,
        node_ids: impl IntoIterator<Item = &'a NodeId>,
        surviving_turn_ids: &HashSet<String>,
    ) -> Result<(), SpineStoreError> {
        for node_id in node_ids {
            self.ensure_node_dir(node_id)?;
            if source
                .latest_memory_source_turn_id(node_id)?
                .is_some_and(|turn_id| surviving_turn_ids.contains(&turn_id))
            {
                self.copy_node_file_if_present(source, node_id, MEMORY_FILE)?;
            }
        }
        Ok(())
    }

    pub(crate) fn copy_projected_compact_index_from(
        &self,
        source: &SpineSidecarStore,
        surviving_compact_ids: &HashSet<String>,
    ) -> Result<(), SpineStoreError> {
        let source_path = source.compact_index_path();
        if !source_path.exists() {
            return Ok(());
        }

        let events = source.read_compact_index_events()?;
        let filtered_events = events
            .into_iter()
            .filter(|event| match event {
                CompactIndexEvent::CompactStarted { compact_id, .. }
                | CompactIndexEvent::MemInstallCommitted { compact_id, .. } => {
                    surviving_compact_ids.contains(compact_id)
                }
                CompactIndexEvent::CompactFailed { .. }
                | CompactIndexEvent::CompactInterrupted { .. } => false,
            })
            .collect::<Vec<_>>();
        self.write_compact_index_events(filtered_events)
    }

    pub(crate) fn validate_root_meminstall_survivors(
        &self,
        surviving_root_compact_ids: &HashSet<String>,
    ) -> Result<(), SpineStoreError> {
        if surviving_root_compact_ids.is_empty() {
            return Ok(());
        }

        self.validate_compact_index()?;

        let mut reset_compact_ids = HashSet::new();
        for event in self.read_tree_events()? {
            if let TreeEvent::RootEpochReset { compact_id, .. } = event {
                reset_compact_ids.insert(compact_id);
            }
        }

        let mut committed_root_compact_ids = HashSet::new();
        for event in self.read_compact_index_events()? {
            if let CompactIndexEvent::MemInstallCommitted { compact_id, op, .. } = event
                && surviving_root_compact_ids.contains(&compact_id)
                && op == SpineOperation::Archive
            {
                committed_root_compact_ids.insert(compact_id);
            }
        }

        for compact_id in surviving_root_compact_ids {
            let root_epoch_reset = reset_compact_ids.contains(compact_id);
            let mem_install_committed = committed_root_compact_ids.contains(compact_id);
            if !root_epoch_reset || !mem_install_committed {
                return Err(SpineStoreError::InvalidLedger(format!(
                    "partial root MemInstall survivor set for compact {compact_id}: root_epoch_reset={root_epoch_reset}, mem_install_committed={mem_install_committed}"
                )));
            }
        }

        Ok(())
    }

    pub(crate) fn validate_mem_install_survivors(
        &self,
        surviving_compact_ids: &HashSet<String>,
    ) -> Result<(), SpineStoreError> {
        if surviving_compact_ids.is_empty() {
            return Ok(());
        }

        let committed_ids = self
            .committed_mem_install_spans_matching_ids(Some(surviving_compact_ids))?
            .into_iter()
            .map(|span| span.compact_id)
            .collect::<HashSet<_>>();
        for compact_id in surviving_compact_ids {
            if !committed_ids.contains(compact_id) {
                return Err(SpineStoreError::InvalidLedger(format!(
                    "surviving Spine compact checkpoint {compact_id} has no committed MemInstall evidence"
                )));
            }
        }
        Ok(())
    }

    pub(crate) fn append_raw_items_recorded(
        &self,
        node_id: &NodeId,
        turn_id: impl Into<String>,
        start: u64,
        end: u64,
    ) -> Result<(), SpineStoreError> {
        let path = self.trajs_index_path();
        let event = TrajsIndexEvent::RawItemsRecorded {
            seq: self.next_jsonl_seq(&path)?,
            node_id: node_id.to_string(),
            turn_id: turn_id.into(),
            start,
            end,
        };
        self.append_json_line(&path, &event)
    }

    pub(crate) fn estimate_raw_response_tokens(
        &self,
        start: u64,
        end: u64,
    ) -> Result<u64, SpineStoreError> {
        if start > end {
            return Err(SpineStoreError::InvalidLedger(format!(
                "raw response token estimate range [{start}, {end}) is invalid"
            )));
        }

        let path = self.raw_rollout_path();
        let file = File::open(&path).map_err(|source| SpineStoreError::Io {
            path: path.clone(),
            source,
        })?;
        let reader = BufReader::new(file);
        let mut response_ordinal = 0_u64;
        let mut chars = 0_u64;
        let mut selected = false;

        for (index, line) in reader.lines().enumerate() {
            let line = line.map_err(|source| SpineStoreError::Io {
                path: path.clone(),
                source,
            })?;
            if line.trim().is_empty() {
                return Err(SpineStoreError::InvalidLedger(format!(
                    "raw rollout line {} is empty",
                    index + 1
                )));
            }
            let row: Value =
                serde_json::from_str(&line).map_err(|source| SpineStoreError::Json {
                    path: path.clone(),
                    source,
                })?;
            if row.get("type").and_then(Value::as_str) != Some("response_item") {
                continue;
            }
            if response_ordinal >= start && response_ordinal < end {
                let payload = row.get("payload").ok_or_else(|| {
                    SpineStoreError::InvalidLedger(format!(
                        "raw rollout response_item line {} is missing payload",
                        index + 1
                    ))
                })?;
                let serialized =
                    serde_json::to_string(payload).map_err(|source| SpineStoreError::Json {
                        path: path.clone(),
                        source,
                    })?;
                let serialized_len = u64::try_from(serialized.len()).map_err(|_| {
                    SpineStoreError::InvalidLedger(
                        "raw rollout response item is too large to estimate".to_string(),
                    )
                })?;
                if selected {
                    chars = chars.checked_add(1).ok_or_else(|| {
                        SpineStoreError::InvalidLedger(
                            "raw response token estimate overflow".to_string(),
                        )
                    })?;
                }
                chars = chars.checked_add(serialized_len).ok_or_else(|| {
                    SpineStoreError::InvalidLedger(
                        "raw response token estimate overflow".to_string(),
                    )
                })?;
                selected = true;
            }
            response_ordinal = response_ordinal.checked_add(1).ok_or_else(|| {
                SpineStoreError::InvalidLedger("raw response ordinal overflow".to_string())
            })?;
            if response_ordinal >= end {
                break;
            }
        }

        if response_ordinal < end {
            return Err(SpineStoreError::InvalidLedger(format!(
                "raw rollout has {response_ordinal} response items, cannot estimate range ending at {end}"
            )));
        }

        Ok(chars.div_ceil(4))
    }

    pub(crate) fn has_size_hint_emitted(
        &self,
        node_id: &NodeId,
        threshold_tokens: u64,
    ) -> Result<bool, SpineStoreError> {
        let node_id = node_id.to_string();
        for event in self.read_tree_events()? {
            let TreeEvent::SpineHintEmitted {
                node_id: event_node_id,
                threshold_tokens: event_threshold_tokens,
                ..
            } = event
            else {
                continue;
            };
            if event_node_id == node_id && event_threshold_tokens == threshold_tokens {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(crate) fn append_size_hint_emitted(
        &self,
        node_id: &NodeId,
        threshold_tokens: u64,
        estimated_tokens: u64,
        source: impl Into<String>,
    ) -> Result<(), SpineStoreError> {
        let event = TreeEvent::SpineHintEmitted {
            seq: self.next_tree_seq()?,
            node_id: node_id.to_string(),
            threshold_tokens,
            estimated_tokens,
            source: source.into(),
        };
        self.append_tree_event(&event)
    }

    pub(crate) fn append_transition_committed(
        &self,
        call_id: impl Into<String>,
        op: SpineOperation,
        from_node: &NodeId,
        to_node: &NodeId,
        call_start_ordinal: u64,
        boundary_end: u64,
    ) -> Result<(), SpineStoreError> {
        let path = self.trajs_index_path();
        let call_id = call_id.into();
        #[cfg(test)]
        if call_id == "__spine_fail_transition_commit__" {
            return Err(SpineStoreError::InvalidLedger(
                "injected transition commit failure".to_string(),
            ));
        }
        let event = TrajsIndexEvent::TransitionCommitted {
            seq: self.next_jsonl_seq(&path)?,
            call_id,
            op,
            from_node: from_node.to_string(),
            to_node: to_node.to_string(),
            call_start_ordinal,
            boundary_end,
        };
        self.append_json_line(&path, &event)
    }

    pub(crate) fn append_raw_mirror_items(
        &self,
        items: &[RolloutItem],
    ) -> Result<(), SpineStoreError> {
        #[cfg(test)]
        if items.iter().any(is_raw_mirror_failure_test_item) {
            return Err(SpineStoreError::InvalidLedger(
                "injected raw mirror failure".to_string(),
            ));
        }
        for item in items {
            self.append_json_line(&self.raw_rollout_path(), item)?;
        }
        Ok(())
    }

    pub(crate) fn append_node_trajs_items(
        &self,
        node_id: &NodeId,
        items: &[RolloutItem],
    ) -> Result<(), SpineStoreError> {
        self.ensure_node_dir(node_id)?;
        let path = self.node_trajs_path(node_id);
        for item in items {
            self.append_json_line(&path, item)?;
        }
        Ok(())
    }

    pub(crate) fn append_compact_started(
        &self,
        record: CompactStartedRecord,
    ) -> Result<(), SpineStoreError> {
        let CompactStartedRecord {
            attempt:
                CompactAttemptRecord {
                    compact_id,
                    node_id,
                    op,
                    cut_ordinal,
                    fold_end_ordinal,
                },
            strategy,
            rollout,
        } = record;
        let path = self.compact_index_path();
        let event = CompactIndexEvent::CompactStarted {
            seq: self.next_jsonl_seq(&path)?,
            compact_id,
            node_id: node_id.to_string(),
            op,
            cut_ordinal,
            fold_end_ordinal,
            strategy,
            rollout,
        };
        self.append_json_line(&path, &event)
    }

    pub(crate) fn append_mem_install_committed(
        &self,
        record: MemInstallCommittedRecord,
    ) -> Result<(), SpineStoreError> {
        let MemInstallCommittedRecord {
            attempt:
                CompactAttemptRecord {
                    compact_id,
                    node_id,
                    op,
                    cut_ordinal,
                    fold_end_ordinal,
                },
            body_ref,
            projection_ref,
            source_rollout_ref,
        } = record;
        let attempt = CompactAttemptRecord {
            compact_id,
            node_id,
            op,
            cut_ordinal,
            fold_end_ordinal,
        };
        self.validate_mem_install_commit_preconditions(
            &attempt,
            &body_ref,
            &projection_ref,
            &source_rollout_ref,
        )?;

        let path = self.compact_index_path();
        let seq = self.next_jsonl_seq(&path)?;
        let memory_section_id = body_ref.section_id.to_string();
        let storage_ref = body_ref.section_id.storage_ref.clone();
        let event = CompactIndexEvent::MemInstallCommitted {
            seq,
            schema_version: MEM_INSTALL_COMMITTED_SCHEMA_VERSION,
            compact_id: attempt.compact_id,
            node_id: attempt.node_id.to_string(),
            op: attempt.op,
            cut_ordinal: attempt.cut_ordinal,
            fold_end_ordinal: attempt.fold_end_ordinal,
            memory_section_id,
            body_hash: body_ref.body_hash,
            storage_ref,
            projection_ref,
            source_rollout_ref,
        };
        self.append_json_line(&path, &event)
    }

    pub(crate) fn append_compact_failed(
        &self,
        record: CompactTerminalRecord,
    ) -> Result<(), SpineStoreError> {
        let CompactTerminalRecord {
            attempt:
                CompactAttemptRecord {
                    compact_id,
                    node_id,
                    op,
                    cut_ordinal,
                    fold_end_ordinal,
                },
            strategy,
            error,
        } = record;
        let path = self.compact_index_path();
        let event = CompactIndexEvent::CompactFailed {
            seq: self.next_jsonl_seq(&path)?,
            compact_id,
            node_id: node_id.to_string(),
            op,
            cut_ordinal,
            fold_end_ordinal,
            strategy,
            error,
        };
        self.append_json_line(&path, &event)
    }

    pub(crate) fn append_compact_interrupted(
        &self,
        record: CompactTerminalRecord,
    ) -> Result<(), SpineStoreError> {
        let CompactTerminalRecord {
            attempt:
                CompactAttemptRecord {
                    compact_id,
                    node_id,
                    op,
                    cut_ordinal,
                    fold_end_ordinal,
                },
            strategy,
            error,
        } = record;
        let path = self.compact_index_path();
        let event = CompactIndexEvent::CompactInterrupted {
            seq: self.next_jsonl_seq(&path)?,
            compact_id,
            node_id: node_id.to_string(),
            op,
            cut_ordinal,
            fold_end_ordinal,
            strategy,
            error,
        };
        self.append_json_line(&path, &event)
    }

    pub(crate) fn append_memory_section(
        &self,
        node_id: &NodeId,
        section: &str,
    ) -> Result<(), SpineStoreError> {
        self.ensure_node_dir(node_id)?;
        let path = self.memory_path(node_id);
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|source| SpineStoreError::Io {
                path: path.clone(),
                source,
            })?;
        file.write_all(GENERATED_MEMORY_SECTION_MARKER.as_bytes())
            .map_err(|source| SpineStoreError::Io {
                path: path.clone(),
                source,
            })?;
        file.write_all(section.as_bytes())
            .map_err(|source| SpineStoreError::Io { path, source })
    }

    pub(crate) fn generated_memory_sections(
        &self,
        node_id: &NodeId,
    ) -> Result<Vec<GeneratedMemorySection>, SpineStoreError> {
        let memory = self.read_memory_file(node_id)?;
        Ok(parse_generated_memory_sections(
            relative_memory_path(node_id),
            &memory,
        ))
    }

    pub(crate) fn verify_memory_body_ref(
        &self,
        node_id: &NodeId,
        body_ref: &MemoryBodyRef,
    ) -> Result<GeneratedMemorySection, SpineStoreError> {
        let memory = self.read_memory_file(node_id)?;
        verify_memory_body_ref_in_memory(relative_memory_path(node_id), &memory, body_ref)
            .map_err(Into::into)
    }

    #[cfg(test)]
    pub(crate) fn read_memory(&self, node_id: &NodeId) -> Result<String, SpineStoreError> {
        self.read_memory_file(node_id)
    }

    fn replay_tree(&self) -> Result<SpineState, SpineStoreError> {
        let events = self.read_tree_events()?;
        let mut state = None;

        for event in events {
            match event {
                TreeEvent::SpineInitialized {
                    initial_raw_start_ordinal,
                    ..
                } => {
                    if state.is_some() {
                        return Err(SpineStoreError::InvalidLedger(
                            "spine was initialized more than once".to_string(),
                        ));
                    }
                    state = Some(SpineState::new_with_initial_leaf_raw_start(
                        initial_raw_start_ordinal,
                    ));
                }
                TreeEvent::TransitionApplied {
                    op,
                    from_node,
                    to_node,
                    summary,
                    raw_start_ordinal,
                    ..
                } => {
                    let state = state.as_mut().ok_or_else(|| {
                        SpineStoreError::InvalidLedger(
                            "transition appeared before root node creation".to_string(),
                        )
                    })?;
                    let from_node = NodeId::parse(&from_node)?;
                    let to_node = NodeId::parse(&to_node)?;
                    let transition = op.apply(state, summary)?;
                    if transition.from != from_node || transition.to != to_node {
                        return Err(SpineStoreError::InvalidLedger(format!(
                            "transition replay mismatch: expected {} -> {}, got {} -> {}",
                            from_node.bracketed(),
                            to_node.bracketed(),
                            transition.from.bracketed(),
                            transition.to.bracketed()
                        )));
                    }
                    state.set_raw_start_ordinal(&transition.to, raw_start_ordinal)?;
                }
                TreeEvent::RootEpochReset {
                    root_id,
                    next_leaf_id,
                    summary,
                    raw_start_ordinal,
                    ..
                } => {
                    let state = state.as_mut().ok_or_else(|| {
                        SpineStoreError::InvalidLedger(
                            "root_epoch_reset appeared before root node creation".to_string(),
                        )
                    })?;
                    let root_id = NodeId::parse(&root_id)?;
                    let next_leaf_id = NodeId::parse(&next_leaf_id)?;
                    let transition = state.reset_root_epoch(summary, raw_start_ordinal)?;
                    if transition.from != root_id || transition.to != next_leaf_id {
                        return Err(SpineStoreError::InvalidLedger(format!(
                            "root epoch reset replay mismatch: expected {} -> {}, got {} -> {}",
                            root_id.bracketed(),
                            next_leaf_id.bracketed(),
                            transition.from.bracketed(),
                            transition.to.bracketed()
                        )));
                    }
                }
                TreeEvent::RawStartOrdinalUpdated {
                    node_id,
                    raw_start_ordinal,
                    ..
                } => {
                    let state = state.as_mut().ok_or_else(|| {
                        SpineStoreError::InvalidLedger(
                            "raw_start_ordinal_updated appeared before spine initialization"
                                .to_string(),
                        )
                    })?;
                    let node_id = NodeId::parse(&node_id)?;
                    state.set_raw_start_ordinal(&node_id, raw_start_ordinal)?;
                }
                TreeEvent::ProjectionReset { checkpoint, .. } => {
                    state = Some(checkpoint.replay()?);
                }
                TreeEvent::SpineHintEmitted { node_id, .. } => {
                    let state = state.as_ref().ok_or_else(|| {
                        SpineStoreError::InvalidLedger(
                            "spine_hint_emitted appeared before root node creation".to_string(),
                        )
                    })?;
                    let node_id = NodeId::parse(&node_id)?;
                    if state.node(&node_id).is_none() {
                        return Err(SpineStoreError::InvalidLedger(format!(
                            "spine_hint_emitted references unknown node {}",
                            node_id.bracketed()
                        )));
                    }
                }
            }
        }

        state.ok_or_else(|| {
            SpineStoreError::InvalidLedger("tree.jsonl does not create a root node".to_string())
        })
    }

    fn read_tree_events(&self) -> Result<Vec<TreeEvent>, SpineStoreError> {
        let path = self.tree_path();
        let file = File::open(&path).map_err(|source| SpineStoreError::Io {
            path: path.clone(),
            source,
        })?;
        let reader = BufReader::new(file);
        let mut events = Vec::new();

        for (index, line) in reader.lines().enumerate() {
            let line = line.map_err(|source| SpineStoreError::Io {
                path: path.clone(),
                source,
            })?;
            if line.trim().is_empty() {
                return Err(SpineStoreError::InvalidLedger(format!(
                    "tree.jsonl line {} is empty",
                    index + 1
                )));
            }
            let event: TreeEvent =
                serde_json::from_str(&line).map_err(|source| SpineStoreError::Json {
                    path: path.clone(),
                    source,
                })?;
            let expected_seq = u64::try_from(index + 1).map_err(|_| {
                SpineStoreError::InvalidLedger("tree.jsonl has too many events".to_string())
            })?;
            if event.seq() != expected_seq {
                return Err(SpineStoreError::InvalidLedger(format!(
                    "tree.jsonl line {} has seq {}, expected {}",
                    index + 1,
                    event.seq(),
                    expected_seq
                )));
            }
            events.push(event);
        }

        self.set_cached_next_tree_seq(next_tree_seq_for_event_count(events.len())?)?;
        Ok(events)
    }

    pub(crate) fn latest_projection_epoch(
        &self,
    ) -> Result<Option<ProjectionEpochMetadata>, SpineStoreError> {
        let mut latest = None;
        for event in self.read_tree_events()? {
            let TreeEvent::ProjectionReset {
                source_rollout_ref,
                processed_rollout_len,
                processed_rollout_hash,
                effective_raw_len,
                surviving_turn_ids_hash,
                surviving_compact_ids,
                checkpoint_hash,
                ..
            } = event
            else {
                continue;
            };
            latest = Some(ProjectionEpochMetadata {
                source_rollout_ref,
                processed_rollout_len,
                processed_rollout_hash,
                effective_raw_len,
                surviving_turn_ids_hash,
                surviving_compact_ids,
                checkpoint_hash,
            });
        }
        Ok(latest)
    }

    fn read_compact_index_events(&self) -> Result<Vec<CompactIndexEvent>, SpineStoreError> {
        let path = self.compact_index_path();
        let file = File::open(&path).map_err(|source| SpineStoreError::Io {
            path: path.clone(),
            source,
        })?;
        let reader = BufReader::new(file);
        let mut events = Vec::new();

        for (index, line) in reader.lines().enumerate() {
            let line = line.map_err(|source| SpineStoreError::Io {
                path: path.clone(),
                source,
            })?;
            if line.trim().is_empty() {
                return Err(SpineStoreError::InvalidLedger(format!(
                    "compact.index.jsonl line {} is empty",
                    index + 1
                )));
            }
            let event: CompactIndexEvent =
                serde_json::from_str(&line).map_err(|source| SpineStoreError::Json {
                    path: path.clone(),
                    source,
                })?;
            let expected_seq = u64::try_from(index + 1).map_err(|_| {
                SpineStoreError::InvalidLedger(
                    "compact.index.jsonl has too many events".to_string(),
                )
            })?;
            if event.seq() != expected_seq {
                return Err(SpineStoreError::InvalidLedger(format!(
                    "compact.index.jsonl line {} has seq {}, expected {}",
                    index + 1,
                    event.seq(),
                    expected_seq
                )));
            }
            events.push(event);
        }

        Ok(events)
    }

    pub(crate) fn committed_mem_installs(
        &self,
    ) -> Result<Vec<CommittedMemInstall>, SpineStoreError> {
        self.validate_compact_index()?;
        let mut installs = Vec::new();
        for event in self.read_compact_index_events()? {
            let CompactIndexEvent::MemInstallCommitted {
                compact_id,
                node_id,
                op,
                cut_ordinal,
                fold_end_ordinal,
                memory_section_id,
                body_hash,
                storage_ref,
                projection_ref,
                source_rollout_ref,
                ..
            } = event
            else {
                continue;
            };
            installs.push(CommittedMemInstall {
                compact_id,
                node_id: NodeId::parse(&node_id)?,
                op,
                cut_ordinal,
                fold_end_ordinal,
                body_ref: MemoryBodyRef {
                    section_id: MemorySectionId::parse(memory_section_id, storage_ref)?,
                    body_hash,
                },
                projection_ref,
                source_rollout_ref,
            });
        }
        Ok(installs)
    }

    #[cfg(test)]
    pub(crate) fn committed_mem_install_spans(
        &self,
    ) -> Result<Vec<InstalledCompactSpan>, SpineStoreError> {
        self.committed_mem_install_spans_matching_ids(None)
    }

    pub(crate) fn committed_mem_install_spans_matching_ids(
        &self,
        surviving_compact_ids: Option<&HashSet<String>>,
    ) -> Result<Vec<InstalledCompactSpan>, SpineStoreError> {
        let mut spans = Vec::new();

        for install in self.committed_mem_installs()? {
            if surviving_compact_ids.is_some_and(|ids| !ids.contains(&install.compact_id)) {
                continue;
            }
            if install.cut_ordinal >= install.fold_end_ordinal {
                return Err(SpineStoreError::InvalidLedger(format!(
                    "compact.index.jsonl committed span for {} is empty or inverted: [{}, {})",
                    install.compact_id, install.cut_ordinal, install.fold_end_ordinal
                )));
            }
            spans.push(InstalledCompactSpan {
                compact_id: install.compact_id,
                node_id: install.node_id,
                op: install.op,
                cut_ordinal: install.cut_ordinal,
                fold_end_ordinal: install.fold_end_ordinal,
            });
        }

        Ok(spans)
    }

    fn validate_mem_install_commit_preconditions(
        &self,
        attempt: &CompactAttemptRecord,
        body_ref: &MemoryBodyRef,
        projection_ref: &str,
        source_rollout_ref: &str,
    ) -> Result<(), SpineStoreError> {
        let mut started_rollout = None;
        let mut duplicate_commit = false;
        let mut terminal_before_commit = None;

        for event in self.read_compact_index_events()? {
            match event {
                CompactIndexEvent::CompactStarted {
                    compact_id,
                    node_id,
                    op,
                    cut_ordinal,
                    fold_end_ordinal,
                    rollout,
                    ..
                } if compact_id == attempt.compact_id => {
                    if node_id != attempt.node_id.to_string()
                        || op != attempt.op
                        || cut_ordinal != attempt.cut_ordinal
                        || fold_end_ordinal != attempt.fold_end_ordinal
                    {
                        return Err(RuntimeFastFailError::MemInstallSpanMismatch {
                            compact_id: attempt.compact_id.clone(),
                        }
                        .into());
                    }
                    if started_rollout.replace(rollout).is_some() {
                        return Err(SpineStoreError::InvalidLedger(format!(
                            "compact.index.jsonl has duplicate compact_started for {}",
                            attempt.compact_id
                        )));
                    }
                }
                CompactIndexEvent::MemInstallCommitted { compact_id, .. }
                    if compact_id == attempt.compact_id =>
                {
                    duplicate_commit = true;
                }
                CompactIndexEvent::CompactFailed { compact_id, .. }
                    if compact_id == attempt.compact_id =>
                {
                    terminal_before_commit = Some("compact_failed");
                }
                CompactIndexEvent::CompactInterrupted { compact_id, .. }
                    if compact_id == attempt.compact_id =>
                {
                    terminal_before_commit = Some("compact_interrupted");
                }
                _ => {}
            }
        }

        if duplicate_commit {
            return Err(RuntimeFastFailError::MemInstallDuplicateCompactId {
                compact_id: attempt.compact_id.clone(),
            }
            .into());
        }
        if let Some(terminal) = terminal_before_commit {
            return Err(RuntimeFastFailError::MemInstallCheckpointBeforeCommit {
                compact_id: attempt.compact_id.clone(),
                terminal,
            }
            .into());
        }
        validate_mem_install_pre_commit(
            &attempt.compact_id,
            started_rollout.is_some(),
            false,
            None,
            projection_ref,
            source_rollout_ref,
            started_rollout
                .as_deref()
                .is_some_and(|rollout| rollout == source_rollout_ref),
        )?;
        match self.verify_memory_body_ref(&attempt.node_id, body_ref) {
            Ok(_) => {}
            Err(SpineStoreError::MemoryBody(err)) => {
                return Err(mem_install_body_error(&attempt.compact_id, err).into());
            }
            Err(err) => return Err(err),
        }
        Ok(())
    }

    fn validate_compact_index(&self) -> Result<(), SpineStoreError> {
        let mut attempts: HashMap<String, CompactAttemptState> = HashMap::new();

        for event in self.read_compact_index_events()? {
            match event {
                CompactIndexEvent::CompactStarted {
                    compact_id,
                    node_id,
                    op,
                    cut_ordinal,
                    fold_end_ordinal,
                    rollout,
                    ..
                } => {
                    if attempts
                        .insert(
                            compact_id.clone(),
                            CompactAttemptState {
                                node_id,
                                op,
                                cut_ordinal,
                                fold_end_ordinal,
                                rollout,
                                terminal: None,
                                mem_install_committed: false,
                            },
                        )
                        .is_some()
                    {
                        return Err(SpineStoreError::InvalidLedger(format!(
                            "compact.index.jsonl has duplicate compact_started for {compact_id}"
                        )));
                    }
                }
                CompactIndexEvent::MemInstallCommitted {
                    seq,
                    schema_version,
                    compact_id,
                    node_id,
                    op,
                    cut_ordinal,
                    fold_end_ordinal,
                    memory_section_id,
                    body_hash,
                    storage_ref,
                    projection_ref,
                    source_rollout_ref,
                } => {
                    record_mem_install_committed(
                        self,
                        &mut attempts,
                        seq,
                        schema_version,
                        compact_id,
                        node_id,
                        op,
                        cut_ordinal,
                        fold_end_ordinal,
                        memory_section_id,
                        body_hash,
                        storage_ref,
                        projection_ref,
                        source_rollout_ref,
                    )?;
                }
                CompactIndexEvent::CompactFailed {
                    compact_id,
                    node_id,
                    op,
                    cut_ordinal,
                    fold_end_ordinal,
                    ..
                } => {
                    record_compact_terminal(
                        &mut attempts,
                        compact_id,
                        node_id,
                        op,
                        cut_ordinal,
                        fold_end_ordinal,
                        "compact_failed",
                    )?;
                }
                CompactIndexEvent::CompactInterrupted {
                    compact_id,
                    node_id,
                    op,
                    cut_ordinal,
                    fold_end_ordinal,
                    ..
                } => {
                    record_compact_terminal(
                        &mut attempts,
                        compact_id,
                        node_id,
                        op,
                        cut_ordinal,
                        fold_end_ordinal,
                        "compact_interrupted",
                    )?;
                }
            }
        }

        for (compact_id, attempt) in attempts {
            if attempt.terminal.is_none() && !attempt.mem_install_committed {
                return Err(SpineStoreError::InvalidLedger(format!(
                    "compact.index.jsonl has dangling compact_started for {compact_id}; failing closed"
                )));
            }
        }

        Ok(())
    }

    fn write_compact_index_events(
        &self,
        events: Vec<CompactIndexEvent>,
    ) -> Result<(), SpineStoreError> {
        let path = self.compact_index_path();
        let mut file = File::create(&path).map_err(|source| SpineStoreError::Io {
            path: path.clone(),
            source,
        })?;
        for (index, mut event) in events.into_iter().enumerate() {
            let seq = u64::try_from(index + 1).map_err(|_| {
                SpineStoreError::InvalidLedger("compact.index.jsonl has too many events".into())
            })?;
            event.set_seq(seq);
            let line = serde_json::to_string(&event).map_err(|source| SpineStoreError::Json {
                path: path.clone(),
                source,
            })?;
            writeln!(file, "{line}").map_err(|source| SpineStoreError::Io {
                path: path.clone(),
                source,
            })?;
        }
        Ok(())
    }

    fn next_tree_seq(&self) -> Result<u64, SpineStoreError> {
        if let Some(next_seq) = self.metadata_cache()?.next_tree_seq {
            return Ok(next_seq);
        }

        let len = self.read_tree_events()?.len();
        next_tree_seq_for_event_count(len)
    }

    pub(crate) fn next_tree_event_seq(&self) -> Result<u64, SpineStoreError> {
        self.next_tree_seq()
    }

    fn next_jsonl_seq(&self, path: &Path) -> Result<u64, SpineStoreError> {
        if !path.exists() {
            return Ok(1);
        }

        let file = File::open(path).map_err(|source| SpineStoreError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let reader = BufReader::new(file);
        let mut count = 0_u64;
        for line in reader.lines() {
            line.map_err(|source| SpineStoreError::Io {
                path: path.to_path_buf(),
                source,
            })?;
            count = count.checked_add(1).ok_or_else(|| {
                SpineStoreError::InvalidLedger(format!("{} has too many events", path.display()))
            })?;
        }
        Ok(count + 1)
    }

    fn append_json_line<T: Serialize>(
        &self,
        path: &Path,
        value: &T,
    ) -> Result<(), SpineStoreError> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|source| SpineStoreError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        serde_json::to_writer(&mut file, value).map_err(|source| SpineStoreError::Json {
            path: path.to_path_buf(),
            source,
        })?;
        file.write_all(b"\n").map_err(|source| SpineStoreError::Io {
            path: path.to_path_buf(),
            source,
        })
    }

    fn append_tree_event(&self, event: &TreeEvent) -> Result<(), SpineStoreError> {
        let expected_seq = self.next_tree_seq()?;
        let event_seq = event.seq();
        if event_seq != expected_seq {
            return Err(SpineStoreError::InvalidLedger(format!(
                "tree event seq {event_seq} does not match next tree seq {expected_seq}"
            )));
        }

        self.append_json_line(&self.tree_path(), event)?;
        self.set_cached_next_tree_seq(next_tree_seq_after(event_seq)?)
    }

    fn metadata_cache(&self) -> Result<MutexGuard<'_, SpineStoreMetadataCache>, SpineStoreError> {
        self.metadata_cache.lock().map_err(|_| {
            SpineStoreError::InvalidLedger("spine store metadata cache lock poisoned".to_string())
        })
    }

    fn set_cached_next_tree_seq(&self, next_seq: u64) -> Result<(), SpineStoreError> {
        self.metadata_cache()?.next_tree_seq = Some(next_seq);
        Ok(())
    }

    fn ensure_sidecar_dir(&self) -> Result<(), SpineStoreError> {
        std::fs::create_dir_all(&self.root).map_err(|source| SpineStoreError::Io {
            path: self.root.clone(),
            source,
        })
    }

    fn ensure_node_dir(&self, node_id: &NodeId) -> Result<(), SpineStoreError> {
        let path = self.node_dir(node_id);
        std::fs::create_dir_all(&path).map_err(|source| SpineStoreError::Io { path, source })
    }

    fn create_trajs_index_file(&self) -> Result<(), SpineStoreError> {
        let path = self.trajs_index_path();
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|source| SpineStoreError::Io {
                path: path.clone(),
                source,
            })?;
        Ok(())
    }

    fn create_compact_index_file(&self) -> Result<(), SpineStoreError> {
        let path = self.compact_index_path();
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|source| SpineStoreError::Io {
                path: path.clone(),
                source,
            })?;
        Ok(())
    }

    fn create_raw_rollout_file(&self) -> Result<(), SpineStoreError> {
        let path = self.raw_rollout_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| SpineStoreError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|source| SpineStoreError::Io {
                path: path.clone(),
                source,
            })?;
        Ok(())
    }

    fn read_memory_file(&self, node_id: &NodeId) -> Result<String, SpineStoreError> {
        let path = self.memory_path(node_id);
        std::fs::read_to_string(&path).map_err(|source| SpineStoreError::Io { path, source })
    }

    fn latest_memory_source_turn_id(
        &self,
        node_id: &NodeId,
    ) -> Result<Option<String>, SpineStoreError> {
        let node_id = node_id.to_string();
        let mut latest_source_turn_id = None;
        for event in self.read_tree_events()? {
            let source_turn_id = match event {
                TreeEvent::TransitionApplied {
                    from_node,
                    source_turn_id,
                    ..
                } if from_node == node_id => Some(source_turn_id),
                TreeEvent::RootEpochReset {
                    root_id,
                    source_turn_id,
                    ..
                } if root_id == node_id => Some(source_turn_id),
                _ => None,
            };
            if let Some(source_turn_id) = source_turn_id {
                latest_source_turn_id = Some(source_turn_id);
            }
        }
        Ok(latest_source_turn_id)
    }

    fn copy_node_file_if_present(
        &self,
        source: &SpineSidecarStore,
        node_id: &NodeId,
        file_name: &str,
    ) -> Result<(), SpineStoreError> {
        let source_path = source.node_dir(node_id).join(file_name);
        if !source_path.exists() {
            return Ok(());
        }
        let destination_path = self.node_dir(node_id).join(file_name);
        if destination_path.exists() {
            return Err(SpineStoreError::InvalidLedger(format!(
                "refusing to overwrite existing spine node artifact {}",
                destination_path.display()
            )));
        }
        std::fs::copy(&source_path, &destination_path).map_err(|source| SpineStoreError::Io {
            path: destination_path,
            source,
        })?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SpineOperation {
    Open,
    Close,
    Archive,
}

#[derive(Clone, Debug)]
pub(crate) struct CompactAttemptRecord {
    pub(crate) compact_id: String,
    pub(crate) node_id: NodeId,
    pub(crate) op: SpineOperation,
    pub(crate) cut_ordinal: u64,
    pub(crate) fold_end_ordinal: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct CompactStartedRecord {
    pub(crate) attempt: CompactAttemptRecord,
    pub(crate) strategy: String,
    pub(crate) rollout: String,
}

#[derive(Clone, Debug)]
pub(crate) struct MemInstallCommittedRecord {
    pub(crate) attempt: CompactAttemptRecord,
    pub(crate) body_ref: MemoryBodyRef,
    pub(crate) projection_ref: String,
    pub(crate) source_rollout_ref: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CommittedMemInstall {
    pub(crate) compact_id: String,
    pub(crate) node_id: NodeId,
    pub(crate) op: SpineOperation,
    pub(crate) cut_ordinal: u64,
    pub(crate) fold_end_ordinal: u64,
    pub(crate) body_ref: MemoryBodyRef,
    pub(crate) projection_ref: String,
    pub(crate) source_rollout_ref: String,
}

#[derive(Clone, Debug)]
pub(crate) struct CompactTerminalRecord {
    pub(crate) attempt: CompactAttemptRecord,
    pub(crate) strategy: String,
    pub(crate) error: String,
}

pub(crate) trait TransitionSummaryArg {
    fn into_transition_summary(self) -> Option<String>;
}

impl TransitionSummaryArg for Option<String> {
    fn into_transition_summary(self) -> Option<String> {
        self
    }
}

impl TransitionSummaryArg for String {
    fn into_transition_summary(self) -> Option<String> {
        Some(self)
    }
}

impl TransitionSummaryArg for &str {
    fn into_transition_summary(self) -> Option<String> {
        Some(self.to_string())
    }
}

impl SpineOperation {
    pub(crate) fn apply(
        self,
        state: &mut SpineState,
        summary: Option<String>,
    ) -> Result<Transition, SpineStateError> {
        match self {
            SpineOperation::Open => {
                if summary.is_some() {
                    return Err(SpineStateError::UnexpectedSummary(SpineOperationName::Open));
                }
                state.open()
            }
            SpineOperation::Close => state
                .close(summary.ok_or(SpineStateError::MissingSummary(SpineOperationName::Close))?),
            SpineOperation::Archive => Err(SpineStateError::ArchiveIsInternal),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum TreeEvent {
    SpineInitialized {
        seq: u64,
        initial_raw_start_ordinal: u64,
    },
    TransitionApplied {
        seq: u64,
        op: SpineOperation,
        from_node: String,
        to_node: String,
        summary: Option<String>,
        raw_start_ordinal: u64,
        source_turn_id: String,
    },
    RootEpochReset {
        seq: u64,
        root_id: String,
        next_leaf_id: String,
        summary: String,
        raw_start_ordinal: u64,
        compact_id: String,
        source_turn_id: String,
    },
    RawStartOrdinalUpdated {
        seq: u64,
        node_id: String,
        raw_start_ordinal: u64,
        source_turn_id: String,
    },
    ProjectionReset {
        seq: u64,
        reason: String,
        source_turn_id: Option<String>,
        source_rollout_ref: String,
        processed_rollout_len: u64,
        processed_rollout_hash: String,
        effective_raw_len: u64,
        surviving_turn_ids_hash: String,
        surviving_compact_ids: Vec<String>,
        checkpoint_hash: String,
        checkpoint: StateCheckpoint,
    },
    SpineHintEmitted {
        seq: u64,
        node_id: String,
        threshold_tokens: u64,
        estimated_tokens: u64,
        source: String,
    },
}

impl TreeEvent {
    fn seq(&self) -> u64 {
        match self {
            TreeEvent::SpineInitialized { seq, .. }
            | TreeEvent::TransitionApplied { seq, .. }
            | TreeEvent::RootEpochReset { seq, .. }
            | TreeEvent::RawStartOrdinalUpdated { seq, .. }
            | TreeEvent::ProjectionReset { seq, .. }
            | TreeEvent::SpineHintEmitted { seq, .. } => *seq,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum TrajsIndexEvent {
    RawItemsRecorded {
        seq: u64,
        node_id: String,
        turn_id: String,
        start: u64,
        end: u64,
    },
    TransitionCommitted {
        seq: u64,
        call_id: String,
        op: SpineOperation,
        from_node: String,
        to_node: String,
        call_start_ordinal: u64,
        boundary_end: u64,
    },
}

#[derive(Debug)]
struct CompactAttemptState {
    node_id: String,
    op: SpineOperation,
    cut_ordinal: u64,
    fold_end_ordinal: u64,
    rollout: String,
    terminal: Option<&'static str>,
    mem_install_committed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InstalledCompactSpan {
    pub(crate) compact_id: String,
    pub(crate) node_id: NodeId,
    pub(crate) op: SpineOperation,
    pub(crate) cut_ordinal: u64,
    pub(crate) fold_end_ordinal: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum CompactIndexEvent {
    CompactStarted {
        seq: u64,
        compact_id: String,
        node_id: String,
        op: SpineOperation,
        cut_ordinal: u64,
        fold_end_ordinal: u64,
        strategy: String,
        rollout: String,
    },
    MemInstallCommitted {
        seq: u64,
        schema_version: u32,
        compact_id: String,
        node_id: String,
        op: SpineOperation,
        cut_ordinal: u64,
        fold_end_ordinal: u64,
        memory_section_id: String,
        body_hash: String,
        storage_ref: String,
        projection_ref: String,
        source_rollout_ref: String,
    },
    CompactFailed {
        seq: u64,
        compact_id: String,
        node_id: String,
        op: SpineOperation,
        cut_ordinal: u64,
        fold_end_ordinal: u64,
        strategy: String,
        error: String,
    },
    CompactInterrupted {
        seq: u64,
        compact_id: String,
        node_id: String,
        op: SpineOperation,
        cut_ordinal: u64,
        fold_end_ordinal: u64,
        strategy: String,
        error: String,
    },
}

impl CompactIndexEvent {
    fn seq(&self) -> u64 {
        match self {
            CompactIndexEvent::CompactStarted { seq, .. }
            | CompactIndexEvent::MemInstallCommitted { seq, .. }
            | CompactIndexEvent::CompactFailed { seq, .. }
            | CompactIndexEvent::CompactInterrupted { seq, .. } => *seq,
        }
    }

    fn set_seq(&mut self, next_seq: u64) {
        match self {
            CompactIndexEvent::CompactStarted { seq, .. }
            | CompactIndexEvent::MemInstallCommitted { seq, .. }
            | CompactIndexEvent::CompactFailed { seq, .. }
            | CompactIndexEvent::CompactInterrupted { seq, .. } => *seq = next_seq,
        }
    }
}

fn next_tree_seq_for_event_count(count: usize) -> Result<u64, SpineStoreError> {
    u64::try_from(count + 1)
        .map_err(|_| SpineStoreError::InvalidLedger("tree.jsonl has too many events".into()))
}

fn next_tree_seq_after(current_seq: u64) -> Result<u64, SpineStoreError> {
    current_seq
        .checked_add(1)
        .ok_or_else(|| SpineStoreError::InvalidLedger("tree.jsonl has too many events".into()))
}

fn record_compact_terminal(
    attempts: &mut HashMap<String, CompactAttemptState>,
    compact_id: String,
    node_id: String,
    op: SpineOperation,
    cut_ordinal: u64,
    fold_end_ordinal: u64,
    terminal: &'static str,
) -> Result<(), SpineStoreError> {
    let Some(attempt) = attempts.get_mut(&compact_id) else {
        return Err(SpineStoreError::InvalidLedger(format!(
            "compact.index.jsonl has {terminal} without matching compact_started for {compact_id}"
        )));
    };
    if attempt.mem_install_committed {
        return Err(RuntimeFastFailError::MemInstallInvalidTerminalAfterCommit {
            compact_id,
            terminal,
        }
        .into());
    }
    if attempt.terminal.is_some() {
        return Err(SpineStoreError::InvalidLedger(format!(
            "compact.index.jsonl has duplicate terminal event for {compact_id}"
        )));
    }
    if attempt.node_id != node_id
        || attempt.op != op
        || attempt.cut_ordinal != cut_ordinal
        || attempt.fold_end_ordinal != fold_end_ordinal
    {
        return Err(SpineStoreError::InvalidLedger(format!(
            "compact.index.jsonl {terminal} does not match compact_started for {compact_id}"
        )));
    }
    attempt.terminal = Some(terminal);
    Ok(())
}

fn record_mem_install_committed(
    store: &SpineSidecarStore,
    attempts: &mut HashMap<String, CompactAttemptState>,
    _seq: u64,
    schema_version: u32,
    compact_id: String,
    node_id: String,
    op: SpineOperation,
    cut_ordinal: u64,
    fold_end_ordinal: u64,
    memory_section_id: String,
    body_hash: String,
    storage_ref: String,
    projection_ref: String,
    source_rollout_ref: String,
) -> Result<(), SpineStoreError> {
    if schema_version != MEM_INSTALL_COMMITTED_SCHEMA_VERSION {
        return Err(RuntimeFastFailError::MemInstallUnsupportedSchema {
            compact_id,
            schema_version,
        }
        .into());
    }

    let Some(attempt) = attempts.get_mut(&compact_id) else {
        return Err(RuntimeFastFailError::MemInstallMissingStarted { compact_id }.into());
    };
    if attempt.mem_install_committed {
        return Err(RuntimeFastFailError::MemInstallDuplicateCompactId { compact_id }.into());
    }
    if let Some(terminal) = attempt.terminal {
        return Err(RuntimeFastFailError::MemInstallCheckpointBeforeCommit {
            compact_id,
            terminal,
        }
        .into());
    }
    if attempt.node_id != node_id
        || attempt.op != op
        || attempt.cut_ordinal != cut_ordinal
        || attempt.fold_end_ordinal != fold_end_ordinal
    {
        return Err(RuntimeFastFailError::MemInstallSpanMismatch { compact_id }.into());
    }
    validate_mem_install_metadata(
        &compact_id,
        &projection_ref,
        &source_rollout_ref,
        attempt.rollout == source_rollout_ref,
    )?;

    let node_id = NodeId::parse(&node_id)?;
    let body_ref = MemoryBodyRef {
        section_id: MemorySectionId::parse(memory_section_id, storage_ref)
            .map_err(|err| mem_install_body_error(&compact_id, err))?,
        body_hash,
    };
    match store.verify_memory_body_ref(&node_id, &body_ref) {
        Ok(_) => {}
        Err(SpineStoreError::MemoryBody(err)) => {
            return Err(mem_install_body_error(&compact_id, err).into());
        }
        Err(err) => return Err(err),
    }
    attempt.mem_install_committed = true;
    Ok(())
}

#[cfg(test)]
fn is_raw_mirror_failure_test_item(item: &RolloutItem) -> bool {
    match item {
        RolloutItem::ResponseItem(codex_protocol::models::ResponseItem::Message {
            content,
            ..
        }) => content.iter().any(|content_item| {
            matches!(
                content_item,
                codex_protocol::models::ContentItem::InputText { text }
                    | codex_protocol::models::ContentItem::OutputText { text }
                    if text == "__spine_fail_raw_mirror__"
            )
        }),
        RolloutItem::EventMsg(codex_protocol::protocol::EventMsg::Warning(warning)) => {
            warning.message == "__spine_fail_raw_mirror__"
        }
        _ => false,
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct SpineBaseLocator {
    version: u32,
    base: String,
}

#[derive(Debug, Error)]
pub(crate) enum SpineStoreError {
    #[error("invalid spine rollout path {path}: {reason}")]
    InvalidRolloutPath { path: PathBuf, reason: &'static str },
    #[error("spine sidecar already initialized at {path}")]
    AlreadyInitialized { path: PathBuf },
    #[error("failed to access spine sidecar file {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse spine sidecar JSON {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("invalid spine ledger: {0}")]
    InvalidLedger(String),
    #[error(transparent)]
    State(#[from] SpineStateError),
    #[error(transparent)]
    StateCheckpoint(#[from] StateCheckpointError),
    #[error(transparent)]
    NodeId(#[from] NodeIdParseError),
    #[error(transparent)]
    MemoryBody(#[from] MemoryBodyError),
    #[error(transparent)]
    RuntimeFastFail(#[from] RuntimeFastFailError),
}

fn rollout_parent(rollout_path: &Path) -> Result<&Path, SpineStoreError> {
    SpineSidecarStore::validate_rollout_path(rollout_path)?;
    rollout_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| SpineStoreError::InvalidRolloutPath {
            path: rollout_path.to_path_buf(),
            reason: "rollout path must include a parent directory",
        })
}

fn rollout_stem(rollout_path: &Path) -> Result<&str, SpineStoreError> {
    SpineSidecarStore::validate_rollout_path(rollout_path)?;
    rollout_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .ok_or_else(|| SpineStoreError::InvalidRolloutPath {
            path: rollout_path.to_path_buf(),
            reason: "rollout path must include a valid UTF-8 file stem",
        })
}

fn read_base_locator(path: &Path) -> Result<SpineBaseLocator, SpineStoreError> {
    let contents = std::fs::read_to_string(path).map_err(|source| SpineStoreError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let locator: SpineBaseLocator =
        serde_json::from_str(&contents).map_err(|source| SpineStoreError::Json {
            path: path.to_path_buf(),
            source,
        })?;
    if locator.version != SPINE_BASE_LOCATOR_VERSION {
        return Err(SpineStoreError::InvalidLedger(format!(
            "unsupported spine base locator version {} at {}",
            locator.version,
            path.display()
        )));
    }
    Ok(locator)
}

fn validate_relative_base(base: &Path, rollout_path: &Path) -> Result<(), SpineStoreError> {
    if base.as_os_str().is_empty() || base.is_absolute() {
        return Err(SpineStoreError::InvalidRolloutPath {
            path: rollout_path.to_path_buf(),
            reason: "spine base locator must contain a non-empty relative base",
        });
    }
    if base.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(SpineStoreError::InvalidRolloutPath {
            path: rollout_path.to_path_buf(),
            reason: "spine base locator must stay within the rollout directory",
        });
    }
    Ok(())
}

fn relative_memory_path(node_id: &NodeId) -> String {
    relative_node_file_path(node_id, MEMORY_FILE)
}

fn relative_node_file_path(node_id: &NodeId, file_name: &str) -> String {
    let mut parts = vec![NODES_DIR.to_string()];
    parts.extend(node_id.segments().iter().map(ToString::to_string));
    parts.push(file_name.to_string());
    parts.join("/")
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
