use crate::spine::SpineError;
use crate::spine::checkpoint::SpineCheckpoint;
use crate::spine::compact_checkpoint::SpineCompactCheckpoint;
use crate::spine::compact_checkpoint::compact_checkpoint_replacement_history_hash;
use crate::spine::compact_checkpoint::validate_compact_checkpoint;
use crate::spine::io::append_json_line;
use crate::spine::io::hash_raw_live;
use crate::spine::io::read_json_file;
use crate::spine::io::read_json_lines;
#[cfg(test)]
use crate::spine::io::write_json_file;
use crate::spine::io::write_json_file_if_unchanged;
use crate::spine::model::LoggedPressureEvent;
use crate::spine::model::LoggedSpineLedgerEvent;
use crate::spine::model::LoggedTrimEvent;
use crate::spine::model::MemRecord;
use crate::spine::model::MemoryContextAccountingRecord;
use crate::spine::model::MemoryContextAccountingWitnessRecord;
#[cfg(test)]
use crate::spine::model::PressureEvent;
use crate::spine::model::RawMask;
use crate::spine::model::SpineCommitMarker;
use crate::spine::model::SpineLedgerEvent;
use crate::spine::model::commit_marker_structural_event_seqs;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs::File;
use std::fs::OpenOptions;
use std::path::Path;
use std::path::PathBuf;

mod checkpoint_proof;
mod clone_rewrite;
mod commit_marker;
mod feedback;
mod locator;
mod memory_body;
mod pressure;
mod trim;

const TREE_FILE: &str = "tree.jsonl";
const PRESSURE_FILE: &str = "pressure.jsonl";
const TRIM_FILE: &str = "trim.jsonl";
const MEM_FILE: &str = "mem.jsonl";
const MEM_ACCOUNTING_FILE: &str = "mem_accounting.jsonl";
const MEM_ACCOUNTING_WITNESS_FILE: &str = "mem_accounting_witness.jsonl";
const COMMIT_FILE: &str = "commits.jsonl";
const COMPACT_CHECKPOINT_FILE: &str = "compact_checkpoints.jsonl";
const FEEDBACK_FILE: &str = "spine_feedback.md";
const WRITER_LOCK_FILE: &str = ".writer.lock";
const CHECKPOINT_DIR: &str = "checkpoints";
const INITIAL_CHECKPOINT_FILE: &str = "initial.json";

pub(crate) const BODY_DIR: &str = "memory";

#[derive(Clone, Debug)]
pub struct SpineCloneBoundary {
    pub(crate) source_rollout_path: PathBuf,
    pub(crate) raw_ordinal_limit: u64,
    pub(crate) structural_seq_limit: u64,
    pub(crate) pressure_seq_watermark: Option<u64>,
    pub(crate) trim_seq_watermark: Option<u64>,
    pub(crate) trim_toolcall_seq_limit: u64,
}

impl SpineCloneBoundary {
    pub(crate) fn raw_ordinal_limit(&self) -> u64 {
        self.raw_ordinal_limit
    }
}

fn event_seq_limit_for_clone(source: &SpineStore) -> Result<u64, SpineError> {
    if source.tree_path().exists() {
        source.next_event_seq()
    } else {
        Ok(0)
    }
}

#[derive(Debug)]
pub(crate) struct SpineStore {
    pub(super) root: PathBuf,
    _writer_lock: Option<File>,
}

impl SpineStore {
    pub(crate) fn for_rollout(rollout_path: &Path) -> Result<Self, SpineError> {
        Ok(Self::from_root(locator::root_for_rollout(rollout_path)?))
    }

    pub(crate) fn create_for_rollout(rollout_path: &Path) -> Result<Self, SpineError> {
        let root = locator::sidecar_root_for_rollout(rollout_path)?;
        std::fs::create_dir_all(&root)?;
        let store = Self::from_root(root);
        store.ensure_trim_ledger_exists()?;
        locator::write_locator_for_root(rollout_path, &store.root)?;
        Ok(store)
    }

    pub(crate) fn load_or_create_for_writer(rollout_path: &Path) -> Result<Self, SpineError> {
        let store = if Self::has_for_rollout(rollout_path)? {
            Self::for_rollout(rollout_path)?
        } else {
            Self::create_for_rollout(rollout_path)?
        };
        store.with_writer_lock()
    }

    pub(crate) fn clone_boundary_for_rollout(
        source_rollout_path: &Path,
        raw_ordinal_limit: u64,
    ) -> Result<Option<SpineCloneBoundary>, SpineError> {
        if !Self::has_for_rollout(source_rollout_path)? {
            return Ok(None);
        }
        let source = Self::for_rollout(source_rollout_path)?;
        let structural_seq_limit = event_seq_limit_for_clone(&source)?;
        let trim_seq_watermark = source.next_trim_seq()?.checked_sub(1);
        Ok(Some(SpineCloneBoundary {
            source_rollout_path: source_rollout_path.to_path_buf(),
            raw_ordinal_limit,
            structural_seq_limit,
            pressure_seq_watermark: source.next_pressure_seq()?.checked_sub(1),
            trim_seq_watermark,
            trim_toolcall_seq_limit: if source.tree_path().exists() {
                structural_seq_limit
            } else {
                source.trim_toolcall_seq_limit(trim_seq_watermark)?
            },
        }))
    }

    pub(crate) fn clone_boundary_for_checkpoint(
        source_rollout_path: &Path,
        raw_ordinal: u64,
    ) -> Result<Option<SpineCloneBoundary>, SpineError> {
        if !Self::has_for_rollout(source_rollout_path)? {
            return Ok(None);
        }
        let source = Self::for_rollout(source_rollout_path)?;
        if !source.tree_path().exists() {
            return source
                .trim_only_clone_boundary_for_raw_ordinal(source_rollout_path, raw_ordinal);
        }
        let checkpoint = source.checkpoint_for_raw_ordinal(raw_ordinal)?;
        let structural_seq_limit = checkpoint.token_seq;
        Ok(Some(SpineCloneBoundary {
            source_rollout_path: source_rollout_path.to_path_buf(),
            raw_ordinal_limit: raw_ordinal,
            structural_seq_limit,
            pressure_seq_watermark: checkpoint.pressure_seq_watermark,
            trim_seq_watermark: checkpoint.trim_seq_watermark,
            trim_toolcall_seq_limit: if source.tree_path().exists() {
                structural_seq_limit
            } else {
                source.trim_toolcall_seq_limit(checkpoint.trim_seq_watermark)?
            },
        }))
    }

    pub(crate) fn clone_for_rollout_with_raw_live(
        boundary: &SpineCloneBoundary,
        target_rollout_path: &Path,
        raw_live: &[bool],
    ) -> Result<(), SpineError> {
        if !Self::has_for_rollout(&boundary.source_rollout_path)? {
            return Ok(());
        }
        if Self::has_for_rollout(target_rollout_path)? {
            return Ok(());
        }
        let raw_ordinal_limit = usize::try_from(boundary.raw_ordinal_limit).map_err(|_| {
            SpineError::InvalidEvent("clone raw ordinal boundary overflow".to_string())
        })?;
        if raw_ordinal_limit > raw_live.len() {
            return Err(SpineError::InvalidEvent(
                "clone raw ordinal boundary exceeds raw live length".to_string(),
            ));
        }
        let source = Self::for_rollout(&boundary.source_rollout_path)?;
        let staging_root = locator::create_unpublished_clone_root(target_rollout_path)?;
        let target_root = staging_root.clone();
        let target = Self::from_root(staging_root.clone());

        let result = clone_for_rollout_into_store(
            &source,
            &target,
            &target_root,
            boundary,
            target_rollout_path,
            raw_live,
            raw_ordinal_limit,
        )
        .and_then(|()| locator::publish_unpublished_clone(target_rollout_path, &staging_root));
        if result.is_err() {
            locator::discard_unpublished_sidecar(&staging_root);
        }
        result
    }

    pub(crate) fn has_for_rollout(rollout_path: &Path) -> Result<bool, SpineError> {
        locator::has_for_rollout(rollout_path)
    }

    fn from_root(root: PathBuf) -> Self {
        Self {
            root,
            _writer_lock: None,
        }
    }

    pub(crate) fn with_writer_lock(mut self) -> Result<Self, SpineError> {
        self.ensure_writer_lock()?;
        Ok(self)
    }

    fn ensure_trim_ledger_exists(&self) -> Result<(), SpineError> {
        if let Some(parent) = self.trim_path().parent() {
            std::fs::create_dir_all(parent)?;
        }
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.trim_path())?;
        Ok(())
    }

    fn trim_only_clone_boundary_for_raw_ordinal(
        &self,
        source_rollout_path: &Path,
        raw_ordinal: u64,
    ) -> Result<Option<SpineCloneBoundary>, SpineError> {
        let trim_events = self.trim_events()?;
        let trim_seq_watermark = trim::seq_watermark_for_raw_boundary(&trim_events, raw_ordinal);
        Ok(Some(SpineCloneBoundary {
            source_rollout_path: source_rollout_path.to_path_buf(),
            raw_ordinal_limit: raw_ordinal,
            structural_seq_limit: 0,
            pressure_seq_watermark: None,
            trim_seq_watermark,
            trim_toolcall_seq_limit: trim::toolcall_seq_limit_from_events(
                &trim_events,
                trim_seq_watermark,
            )?,
        }))
    }

    fn trim_toolcall_seq_limit(&self, trim_seq_watermark: Option<u64>) -> Result<u64, SpineError> {
        trim::toolcall_seq_limit_from_events(&self.trim_events()?, trim_seq_watermark)
    }

    pub(crate) fn ensure_writer_lock(&mut self) -> Result<(), SpineError> {
        if self._writer_lock.is_some() {
            return Ok(());
        }
        std::fs::create_dir_all(&self.root)?;
        let lock_path = self.root.join(WRITER_LOCK_FILE);
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&lock_path)?;
        match lock.try_lock() {
            Ok(()) => {
                self._writer_lock = Some(lock);
                Ok(())
            }
            Err(std::fs::TryLockError::WouldBlock) => Err(SpineError::InvalidStore(format!(
                "Spine sidecar {} is already owned by another live Codex process",
                self.root.display()
            ))),
            Err(std::fs::TryLockError::Error(err)) => Err(err.into()),
        }
    }
}

fn clone_for_rollout_into_store(
    source: &SpineStore,
    target: &SpineStore,
    target_root: &Path,
    boundary: &SpineCloneBoundary,
    target_rollout_path: &Path,
    raw_live: &[bool],
    raw_ordinal_limit: usize,
) -> Result<(), SpineError> {
    let source_raw_live = &raw_live[..raw_ordinal_limit];
    let mask = RawMask::new(source_raw_live);
    target.ensure_trim_ledger_exists()?;
    let clone_jit_records = source.tree_path().exists();
    let source_events = if clone_jit_records {
        source.events()?
    } else {
        Vec::new()
    };
    let source_mems = source.mems()?;
    let source_checkpoints = if clone_jit_records {
        source.checkpoints()?
    } else {
        Vec::new()
    };
    let source_compact_checkpoints = if clone_jit_records {
        source.compact_checkpoints()?
    } else {
        Vec::new()
    };
    let source_commit_markers = if clone_jit_records {
        source.commit_markers()?
    } else {
        Vec::new()
    };
    let source_trim_events = source.trim_events()?;
    let source_events_by_seq = source_events
        .iter()
        .map(|event| (event.seq, event))
        .collect::<BTreeMap<_, _>>();
    let mut cloned_checkpoints = Vec::new();
    for checkpoint in source_checkpoints {
        let checkpoint_boundary = usize::try_from(checkpoint.raw_ordinal)
            .map_err(|_| SpineError::InvalidEvent("checkpoint raw ordinal overflow".to_string()))?;
        if checkpoint.checkpoint_id != "initial"
            && checkpoint.token_seq <= boundary.structural_seq_limit
            && checkpoint.raw_ordinal <= boundary.raw_ordinal_limit
            && checkpoint_boundary <= source_raw_live.len()
            && checkpoint.raw_live_hash == hash_raw_live(&source_raw_live[..checkpoint_boundary])
        {
            cloned_checkpoints.push(checkpoint);
        }
    }
    let mut cloned_compact_checkpoints = Vec::new();
    for checkpoint in source_compact_checkpoints {
        let checkpoint_boundary = usize::try_from(checkpoint.raw_boundary).map_err(|_| {
            SpineError::InvalidEvent("compact checkpoint raw boundary overflow".to_string())
        })?;
        if checkpoint.token_seq <= boundary.structural_seq_limit
            && checkpoint.raw_boundary <= boundary.raw_ordinal_limit
            && checkpoint_boundary <= source_raw_live.len()
            && checkpoint.raw_live_hash == hash_raw_live(&source_raw_live[..checkpoint_boundary])
        {
            cloned_compact_checkpoints.push(checkpoint);
        }
    }
    let mut all_marker_structural_event_seqs = BTreeSet::new();
    let mut cloned_commit_markers = Vec::new();
    for marker in source_commit_markers {
        commit_marker::validate_commit_marker_record(&marker)?;
        commit_marker::validate_commit_marker_events(&marker, &source_events_by_seq)?;
        let structural_event_seqs = commit_marker_structural_event_seqs(&marker)?;
        all_marker_structural_event_seqs.extend(structural_event_seqs.iter().copied());
        let marker_in_clone_boundary = marker.token_seq_end <= boundary.structural_seq_limit
            && marker.raw_boundary <= boundary.raw_ordinal_limit;
        if !marker_in_clone_boundary {
            continue;
        }
        if !commit_marker::commit_marker_allowed_by_source_live(&marker, source_raw_live)? {
            return Err(SpineError::InvalidStore(format!(
                "Spine commit marker {} is not proved by clone raw live state",
                marker.op_id
            )));
        }
        for seq in (marker.token_seq_start..marker.token_seq_end)
            .filter(|seq| !structural_event_seqs.contains(seq))
        {
            let Some(event) = source_events_by_seq.get(&seq) else {
                return Err(SpineError::InvalidStore(format!(
                    "Spine commit marker {} references missing raw-backed event at token_seq {}",
                    marker.op_id, seq
                )));
            };
            if !event.allowed_by(mask)? {
                return Err(SpineError::InvalidStore(format!(
                    "Spine commit marker {} raw-backed event at token_seq {} is not proved by clone raw live state",
                    marker.op_id, seq
                )));
            }
        }
        cloned_commit_markers.push(marker);
    }
    drop(source_events_by_seq);
    let mut marker_proved_event_seqs = BTreeSet::new();
    for marker in &cloned_commit_markers {
        marker_proved_event_seqs.extend(commit_marker_structural_event_seqs(marker)?);
    }
    let mut cloned_events = Vec::new();
    for event in source_events {
        if event.seq >= boundary.structural_seq_limit {
            continue;
        }
        if marker_proved_event_seqs.contains(&event.seq) {
            cloned_events.push(event);
        } else if !all_marker_structural_event_seqs.contains(&event.seq)
            && event.allowed_by(mask)?
        {
            cloned_events.push(event);
        }
    }
    for event in &cloned_events {
        target.append_logged_event(event)?;
    }
    let mut required_memory_ids =
        required_memory_ids_for_cloned_events(&cloned_events, &source_mems, mask)?;
    for checkpoint in &cloned_compact_checkpoints {
        for memory in &checkpoint.memory_refs {
            required_memory_ids.insert(memory.compact_id.clone());
        }
    }
    for checkpoint in &cloned_checkpoints {
        for memory in &checkpoint.memory_refs {
            required_memory_ids.insert(memory.compact_id.clone());
        }
    }
    for marker in &cloned_commit_markers {
        for memory in &marker.memory_refs {
            required_memory_ids.insert(memory.compact_id.clone());
        }
    }
    for pressure in source.pressure_events()? {
        if boundary
            .pressure_seq_watermark
            .is_some_and(|watermark| pressure.pressure_seq <= watermark)
            && pressure.allowed_by(source_raw_live)
        {
            target.append_logged_pressure_event(&pressure)?;
        }
    }
    for trim in source_trim_events {
        if boundary
            .trim_seq_watermark
            .is_some_and(|watermark| trim.trim_seq <= watermark)
            && trim.allowed_by(mask)?
            && trim::event_within_toolcall_boundary(&trim, boundary.trim_toolcall_seq_limit)
        {
            target.append_logged_trim_event(&trim)?;
        }
    }
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
    for checkpoint in cloned_compact_checkpoints {
        let checkpoint = clone_rewrite::clone_compact_checkpoint_for_target(
            checkpoint,
            target_rollout_path,
            &cloned_memory_paths,
        )?;
        target.append_compact_checkpoint(&checkpoint)?;
    }
    for checkpoint in cloned_checkpoints {
        let checkpoint = clone_rewrite::clone_checkpoint_for_target(
            checkpoint,
            target_rollout_path,
            target_root,
            &cloned_memory_paths,
        )?;
        target.write_checkpoint(&checkpoint)?;
    }
    for marker in cloned_commit_markers {
        let marker = clone_rewrite::clone_commit_marker_for_target(marker, &cloned_memory_paths)?;
        target.append_commit_marker(&marker)?;
    }
    Ok(())
}

impl SpineStore {
    pub(super) fn tree_path(&self) -> PathBuf {
        self.root.join(TREE_FILE)
    }

    #[cfg(test)]
    pub(crate) fn tree_path_for_test(&self) -> PathBuf {
        self.tree_path()
    }

    pub(super) fn mem_path(&self) -> PathBuf {
        self.root.join(MEM_FILE)
    }

    fn mem_accounting_path(&self) -> PathBuf {
        self.root.join(MEM_ACCOUNTING_FILE)
    }

    fn mem_accounting_witness_path(&self) -> PathBuf {
        self.root.join(MEM_ACCOUNTING_WITNESS_FILE)
    }

    fn commit_path(&self) -> PathBuf {
        self.root.join(COMMIT_FILE)
    }

    #[cfg(test)]
    pub(crate) fn commit_path_for_test(&self) -> PathBuf {
        self.commit_path()
    }

    pub(super) fn pressure_path(&self) -> PathBuf {
        self.root.join(PRESSURE_FILE)
    }

    #[cfg(test)]
    pub(crate) fn pressure_path_for_test(&self) -> PathBuf {
        self.pressure_path()
    }

    pub(super) fn trim_path(&self) -> PathBuf {
        self.root.join(TRIM_FILE)
    }

    #[cfg(test)]
    pub(crate) fn trim_path_for_test(&self) -> PathBuf {
        self.trim_path()
    }

    pub(crate) fn feedback_path(&self) -> PathBuf {
        self.root.join(FEEDBACK_FILE)
    }

    #[cfg(test)]
    pub(crate) fn feedback_path_for_test(&self) -> PathBuf {
        self.feedback_path()
    }

    fn compact_checkpoint_path(&self) -> PathBuf {
        self.root.join(COMPACT_CHECKPOINT_FILE)
    }

    #[cfg(test)]
    pub(crate) fn compact_checkpoint_path_for_test(&self) -> PathBuf {
        self.compact_checkpoint_path()
    }

    fn checkpoint_dir(&self) -> PathBuf {
        self.root.join(CHECKPOINT_DIR)
    }

    pub(super) fn checkpoint_path(&self, raw_ordinal: u64) -> PathBuf {
        self.checkpoint_dir()
            .join(format!("pre-user-{raw_ordinal:020}.json"))
    }

    pub(super) fn initial_checkpoint_path(&self) -> PathBuf {
        self.checkpoint_dir().join(INITIAL_CHECKPOINT_FILE)
    }

    #[cfg(test)]
    pub(crate) fn initial_checkpoint_path_for_test(&self) -> PathBuf {
        self.initial_checkpoint_path()
    }

    pub(super) fn append_event(&self, event: &SpineLedgerEvent) -> Result<u64, SpineError> {
        let seq = self.next_event_seq()?;
        self.append_logged_event(&LoggedSpineLedgerEvent {
            seq,
            event: event.clone(),
        })?;
        Ok(seq)
    }

    pub(super) fn append_logged_event(
        &self,
        event: &LoggedSpineLedgerEvent,
    ) -> Result<(), SpineError> {
        append_json_line(&self.tree_path(), event)
    }

    #[cfg(test)]
    pub(super) fn append_pressure_event(&self, event: &PressureEvent) -> Result<u64, SpineError> {
        let pressure_seq = self.next_pressure_seq()?;
        self.append_logged_pressure_event(&LoggedPressureEvent {
            pressure_seq,
            event: event.clone(),
        })?;
        Ok(pressure_seq)
    }

    pub(super) fn append_logged_pressure_event(
        &self,
        event: &LoggedPressureEvent,
    ) -> Result<(), SpineError> {
        pressure::append_json_line(&self.pressure_path(), event)
    }

    pub(super) fn append_logged_trim_event(
        &self,
        event: &LoggedTrimEvent,
    ) -> Result<(), SpineError> {
        append_json_line(&self.trim_path(), event)
    }

    pub(super) fn append_mem(&self, mem: &MemRecord) -> Result<(), SpineError> {
        append_json_line(&self.mem_path(), mem)
    }

    pub(super) fn append_mem_accounting(
        &self,
        accounting: &MemoryContextAccountingRecord,
    ) -> Result<(), SpineError> {
        append_json_line(&self.mem_accounting_path(), accounting)
    }

    pub(super) fn append_mem_accounting_witness(
        &self,
        witness: &MemoryContextAccountingWitnessRecord,
    ) -> Result<(), SpineError> {
        append_json_line(&self.mem_accounting_witness_path(), witness)
    }

    pub(super) fn append_commit_marker(
        &self,
        marker: &SpineCommitMarker,
    ) -> Result<(), SpineError> {
        append_json_line(&self.commit_path(), marker)
    }

    pub(super) fn append_compact_checkpoint(
        &self,
        checkpoint: &SpineCompactCheckpoint,
    ) -> Result<(), SpineError> {
        append_json_line(&self.compact_checkpoint_path(), checkpoint)
    }

    pub(crate) fn append_feedback_markdown(&self, entry: &str) -> Result<(), SpineError> {
        feedback::append_markdown_entry(&self.feedback_path(), entry)
    }

    pub(super) fn events(&self) -> Result<Vec<LoggedSpineLedgerEvent>, SpineError> {
        read_json_lines(&self.tree_path())
    }

    pub(super) fn pressure_events(&self) -> Result<Vec<LoggedPressureEvent>, SpineError> {
        if !self.pressure_path().exists() {
            return Ok(Vec::new());
        }
        pressure::read_json_lines(&self.pressure_path())
    }

    pub(super) fn trim_events(&self) -> Result<Vec<LoggedTrimEvent>, SpineError> {
        if !self.trim_path().exists() {
            return Err(SpineError::InvalidStore(format!(
                "missing required Spine trim ledger: {}",
                self.trim_path().display()
            )));
        }
        read_json_lines(&self.trim_path())
    }

    #[cfg(test)]
    pub(super) fn events_for_test(&self) -> Result<Vec<LoggedSpineLedgerEvent>, SpineError> {
        self.events()
    }

    #[cfg(test)]
    pub(crate) fn event_count_for_test(&self) -> Result<usize, SpineError> {
        Ok(self.events()?.len())
    }

    #[cfg(test)]
    pub(crate) fn suffix_mem_cover_for_test(
        &self,
        node_path: &str,
    ) -> Result<Option<(u64, u64, usize, usize)>, SpineError> {
        Ok(self
            .mems()?
            .into_iter()
            .find(|mem| mem.node.as_path() == node_path)
            .map(|mem| {
                (
                    mem.raw_start,
                    mem.raw_end,
                    mem.context_start,
                    mem.context_end,
                )
            }))
    }

    #[cfg(test)]
    pub(crate) fn memory_body_for_test(
        &self,
        node_path: &str,
    ) -> Result<Option<String>, SpineError> {
        self.mems()?
            .into_iter()
            .find(|mem| mem.node.as_path() == node_path)
            .map(|mem| self.read_memory_body(&mem))
            .transpose()
    }

    fn checkpoint_for_raw_ordinal(&self, raw_ordinal: u64) -> Result<SpineCheckpoint, SpineError> {
        read_json_file(&self.checkpoint_path(raw_ordinal))
    }

    #[cfg(test)]
    pub(super) fn checkpoint_for_test(
        &self,
        raw_ordinal: u64,
    ) -> Result<SpineCheckpoint, SpineError> {
        self.checkpoint_for_raw_ordinal(raw_ordinal)
    }

    #[cfg(test)]
    pub(super) fn initial_checkpoint_for_test(&self) -> Result<SpineCheckpoint, SpineError> {
        read_json_file(&self.initial_checkpoint_path())
    }

    #[cfg(test)]
    pub(crate) fn initial_checkpoint_identity_for_test(
        &self,
    ) -> Result<(String, String), SpineError> {
        let checkpoint: SpineCheckpoint = read_json_file(&self.initial_checkpoint_path())?;
        Ok((checkpoint.checkpoint_id, checkpoint.cursor))
    }

    pub(super) fn next_event_seq(&self) -> Result<u64, SpineError> {
        if !self.tree_path().exists() {
            return Ok(0);
        }
        Ok(self
            .events()?
            .into_iter()
            .map(|event| event.seq)
            .max()
            .map(|seq| {
                seq.checked_add(1)
                    .ok_or_else(|| SpineError::InvalidEvent("spine event seq overflow".to_string()))
            })
            .transpose()?
            .unwrap_or(0))
    }

    pub(super) fn next_pressure_seq(&self) -> Result<u64, SpineError> {
        if !self.pressure_path().exists() {
            return Ok(0);
        }
        Ok(self
            .pressure_events()?
            .into_iter()
            .map(|event| event.pressure_seq)
            .max()
            .map(|pressure_seq| {
                pressure_seq.checked_add(1).ok_or_else(|| {
                    SpineError::InvalidEvent("spine pressure seq overflow".to_string())
                })
            })
            .transpose()?
            .unwrap_or(0))
    }

    pub(super) fn next_trim_seq(&self) -> Result<u64, SpineError> {
        Ok(self
            .trim_events()?
            .into_iter()
            .map(|event| event.trim_seq)
            .max()
            .map(|trim_seq| {
                trim_seq
                    .checked_add(1)
                    .ok_or_else(|| SpineError::InvalidEvent("spine trim seq overflow".to_string()))
            })
            .transpose()?
            .unwrap_or(0))
    }

    pub(super) fn mems(&self) -> Result<Vec<MemRecord>, SpineError> {
        if !self.mem_path().exists() {
            return Ok(Vec::new());
        }
        read_json_lines(&self.mem_path())
    }

    pub(super) fn mem_accounting(&self) -> Result<Vec<MemoryContextAccountingRecord>, SpineError> {
        if !self.mem_accounting_path().exists() {
            return Ok(Vec::new());
        }
        read_json_lines(&self.mem_accounting_path())
    }

    pub(super) fn mem_accounting_witnesses(
        &self,
    ) -> Result<Vec<MemoryContextAccountingWitnessRecord>, SpineError> {
        if !self.mem_accounting_witness_path().exists() {
            return Ok(Vec::new());
        }
        read_json_lines(&self.mem_accounting_witness_path())
    }

    pub(super) fn commit_markers(&self) -> Result<Vec<SpineCommitMarker>, SpineError> {
        if !self.commit_path().exists() {
            return Ok(Vec::new());
        }
        read_json_lines(&self.commit_path())
    }

    #[cfg(test)]
    pub(super) fn commit_markers_for_test(&self) -> Result<Vec<SpineCommitMarker>, SpineError> {
        self.commit_markers()
    }

    #[cfg(test)]
    pub(crate) fn mem_close_tokens_for_test(
        &self,
    ) -> Result<Vec<(Option<i64>, Option<i64>)>, SpineError> {
        Ok(self
            .mems()?
            .into_iter()
            .map(|mem| (mem.close_input_tokens, mem.close_context_tokens))
            .collect())
    }

    #[cfg(test)]
    pub(crate) fn root_compact_next_open_tokens_for_test(
        &self,
    ) -> Result<Vec<(Option<i64>, Option<i64>)>, SpineError> {
        Ok(self
            .events()?
            .into_iter()
            .filter_map(|event| match event.event {
                SpineLedgerEvent::RootCompact {
                    next_open_input_tokens,
                    next_open_context_tokens,
                    ..
                } => Some((next_open_input_tokens, next_open_context_tokens)),
                _ => None,
            })
            .collect())
    }

    pub(super) fn compact_checkpoints(&self) -> Result<Vec<SpineCompactCheckpoint>, SpineError> {
        if !self.compact_checkpoint_path().exists() {
            return Ok(Vec::new());
        }
        read_json_lines(&self.compact_checkpoint_path())
    }

    pub(crate) fn validate_compact_checkpoint_for_boundary(
        &self,
        rollout_path: &Path,
        raw_live: &[bool],
        raw_items: &[Option<codex_protocol::models::ResponseItem>],
        raw_boundary: u64,
        replacement_history: &[codex_protocol::models::ResponseItem],
    ) -> Result<u64, SpineError> {
        let replacement_history_hash =
            compact_checkpoint_replacement_history_hash(replacement_history)?;
        let checkpoints = self
            .compact_checkpoints()?
            .into_iter()
            .filter(|checkpoint| checkpoint.raw_boundary == raw_boundary)
            .collect::<Vec<_>>();
        if checkpoints.is_empty() {
            return Err(SpineError::InvalidStore(format!(
                "missing spine compact checkpoint at raw boundary {raw_boundary}"
            )));
        }
        let checkpoints = checkpoints
            .into_iter()
            .filter(|checkpoint| {
                checkpoint.replacement_history_hash == replacement_history_hash
                    && checkpoint.h_ps_hash == replacement_history_hash
            })
            .collect::<Vec<_>>();
        if checkpoints.is_empty() {
            return Err(SpineError::InvalidStore(format!(
                "spine_jit replacement_history does not match sidecar h(PS) compact checkpoint at raw boundary {raw_boundary}"
            )));
        }
        let token_seqs = checkpoints
            .iter()
            .map(|checkpoint| checkpoint.token_seq)
            .collect::<BTreeSet<_>>();
        if token_seqs.len() != 1 {
            return Err(SpineError::InvalidStore(format!(
                "ambiguous spine compact checkpoint token_seq for raw boundary {raw_boundary}"
            )));
        }
        if checkpoints.len() != 1 {
            return Err(SpineError::InvalidStore(format!(
                "ambiguous spine compact checkpoint proof for raw boundary {raw_boundary}"
            )));
        }
        let checkpoint = checkpoints
            .into_iter()
            .next()
            .expect("checkpoint length checked above");
        validate_compact_checkpoint(
            &checkpoint,
            rollout_path,
            raw_live,
            raw_items,
            replacement_history,
        )?;
        let events = self.events()?;
        let mems = self.mems()?;
        checkpoint_proof::validate_compact_checkpoint_root_marker(
            &self.root,
            &checkpoint,
            &events,
            &mems,
        )?;
        checkpoint_proof::validate_compact_checkpoint_memory_refs(&self.root, &checkpoint, &mems)?;
        Ok(checkpoint.token_seq)
    }

    fn checkpoints(&self) -> Result<Vec<SpineCheckpoint>, SpineError> {
        let dir = self.checkpoint_dir();
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut paths = std::fs::read_dir(&dir)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<Vec<_>, _>>()?;
        paths.sort();
        paths
            .into_iter()
            .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
            .map(|path| read_json_file(&path))
            .collect()
    }

    pub(super) fn write_checkpoint(&self, checkpoint: &SpineCheckpoint) -> Result<(), SpineError> {
        let path = self.checkpoint_path(checkpoint.raw_ordinal);
        write_json_file_if_unchanged(&path, checkpoint)
    }

    pub(super) fn write_initial_checkpoint(
        &self,
        checkpoint: &SpineCheckpoint,
    ) -> Result<(), SpineError> {
        write_json_file_if_unchanged(&self.initial_checkpoint_path(), checkpoint)
    }

    pub(super) fn rollback_checkpoint(
        &self,
        rollback_cuts: &[usize],
    ) -> Result<Option<SpineCheckpoint>, SpineError> {
        let Some(cut) = rollback_cuts.iter().min().copied() else {
            return Ok(None);
        };
        let cut = u64::try_from(cut)
            .map_err(|_| SpineError::InvalidEvent("rollback cut overflow".to_string()))?;
        self.checkpoints()?
            .into_iter()
            .find(|checkpoint| checkpoint.raw_ordinal == cut)
            .map(Some)
            .ok_or_else(|| {
                SpineError::InvalidStore(format!(
                    "missing spine rollback checkpoint before raw ordinal {cut}"
                ))
            })
    }

    pub(super) fn resume_checkpoint(
        &self,
        raw_boundary: usize,
    ) -> Result<Option<SpineCheckpoint>, SpineError> {
        let raw_boundary = u64::try_from(raw_boundary)
            .map_err(|_| SpineError::InvalidEvent("resume raw boundary overflow".to_string()))?;
        Ok(self
            .checkpoints()?
            .into_iter()
            .filter(|checkpoint| checkpoint.checkpoint_id != "initial")
            .filter(|checkpoint| checkpoint.raw_ordinal <= raw_boundary)
            .max_by_key(|checkpoint| (checkpoint.raw_ordinal, checkpoint.token_seq)))
    }

    #[cfg(test)]
    pub(crate) fn corrupt_latest_resume_checkpoint_h_ps_hash_for_test(
        &self,
        raw_boundary: usize,
    ) -> Result<u64, SpineError> {
        let mut checkpoint = self
            .resume_checkpoint(raw_boundary)?
            .ok_or_else(|| SpineError::InvalidStore("missing resume checkpoint".to_string()))?;
        checkpoint.h_ps_hash = "bad-hash".to_string();
        let raw_ordinal = checkpoint.raw_ordinal;
        write_json_file(&self.checkpoint_path(raw_ordinal), &checkpoint)?;
        Ok(raw_ordinal)
    }

    pub(super) fn write_memory_body(
        &self,
        compact_id: &str,
        body: &str,
    ) -> Result<String, SpineError> {
        memory_body::write_body(&self.root, compact_id, body)
    }

    pub(super) fn read_memory_body(&self, mem: &MemRecord) -> Result<String, SpineError> {
        memory_body::read_body(&self.root, mem)
    }

    pub(super) fn validate_commit_markers_for_replay(
        &self,
        events: &[LoggedSpineLedgerEvent],
        mems: &[MemRecord],
        raw_live: &[bool],
        min_seq: Option<u64>,
        max_seq: Option<u64>,
    ) -> Result<(), SpineError> {
        let markers = self.commit_markers()?;
        commit_marker::validate_markers_for_replay(
            &self.root, &markers, events, mems, raw_live, min_seq, max_seq,
        )
    }
}

fn sidecar_store_path(store_root: &Path, path: &str) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        store_root.join(path)
    }
}

fn required_memory_ids_for_cloned_events(
    events: &[LoggedSpineLedgerEvent],
    mems: &[MemRecord],
    raw_mask: RawMask<'_>,
) -> Result<BTreeSet<String>, SpineError> {
    let mut ids = BTreeSet::new();
    for event in events {
        match &event.event {
            SpineLedgerEvent::Close { node, .. } => {
                let mut candidates = mems
                    .iter()
                    .filter(|mem| &mem.node == node)
                    .collect::<Vec<_>>();
                candidates.sort_by(|left, right| left.compact_id.cmp(&right.compact_id));
                let mut selected = None;
                for mem in candidates {
                    if mem.allowed_by(raw_mask)? {
                        selected = Some(mem);
                        break;
                    }
                }
                let mem = selected.ok_or_else(|| {
                    SpineError::InvalidEvent(format!("missing memory for close node {node}"))
                })?;
                ids.insert(mem.compact_id.clone());
            }
            SpineLedgerEvent::RootCompact { mem, .. } => {
                let mem_record = mems
                    .iter()
                    .find(|record| record.compact_id == *mem)
                    .ok_or_else(|| {
                        SpineError::InvalidEvent("missing memory for root compact".to_string())
                    })?;
                if !mem_record.allowed_by(raw_mask)? {
                    return Err(SpineError::InvalidEvent(format!(
                        "memory {} does not cover live raw evidence",
                        mem_record.compact_id
                    )));
                }
                ids.insert(mem.clone());
            }
            SpineLedgerEvent::Init { .. }
            | SpineLedgerEvent::Msg { .. }
            | SpineLedgerEvent::ToolCall { .. }
            | SpineLedgerEvent::Open { .. }
            | SpineLedgerEvent::OpenContextBaseline { .. } => {}
        }
    }
    Ok(ids)
}
