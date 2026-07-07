use crate::spine::SpineError;
use crate::spine::model::LoggedSpineLedgerEvent;
use crate::spine::model::MemRecord;
use std::fs::File;
use std::path::Path;
use std::path::PathBuf;

mod checkpoint_proof;
mod checkpoints;
mod clone_rewrite;
mod clone_sidecar;
mod commit_marker;
mod compact_validation;
mod ledger;
mod locator;
mod mem_lookup;
mod memory_body;
mod paths;
mod pressure;
#[cfg(test)]
mod test_support;
mod trim;
mod writer_lock;

#[cfg(test)]
pub(crate) use clone_sidecar::SnapshotTurnState;
pub(crate) use paths::BODY_DIR;
#[cfg(test)]
pub(crate) use clone_sidecar::SnapshotTurnState;
use paths::sidecar_store_path;

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

    pub(crate) fn has_for_rollout(rollout_path: &Path) -> Result<bool, SpineError> {
        locator::has_for_rollout(rollout_path)
    }

    pub(crate) fn debug_request_dir_for_rollout(
        rollout_path: &Path,
    ) -> Result<PathBuf, SpineError> {
        let root = if Self::has_for_rollout(rollout_path)? {
            locator::root_for_rollout(rollout_path)?
        } else {
            locator::sidecar_root_for_rollout(rollout_path)?
        };
        Ok(root.join("debug_request"))
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
        trim::ensure_ledger_exists(&self.trim_path())
    }

    pub(crate) fn ensure_writer_lock(&mut self) -> Result<(), SpineError> {
        if self._writer_lock.is_some() {
            return Ok(());
        }
        self._writer_lock = Some(writer_lock::acquire(&self.root)?);
        Ok(())
    }
}

impl SpineStore {
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

    pub(super) fn memory_body_path(&self, mem: &MemRecord) -> PathBuf {
        sidecar_store_path(&self.root, &mem.body_path)
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
