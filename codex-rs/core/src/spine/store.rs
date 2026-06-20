use crate::spine::SpineError;
use crate::spine::model::LoggedSpineLedgerEvent;
use crate::spine::model::MemRecord;
#[cfg(test)]
use crate::spine::model::SpineCommitMarker;
#[cfg(test)]
use crate::spine::model::SpineLedgerEvent;
use std::fs::File;
use std::path::Path;
use std::path::PathBuf;

mod checkpoint_proof;
mod checkpoints;
mod clone_rewrite;
mod clone_sidecar;
mod commit_marker;
mod compact_validation;
mod feedback;
mod ledger;
mod locator;
mod memory_body;
mod paths;
mod pressure;
mod trim;
mod writer_lock;

pub(crate) use paths::BODY_DIR;
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
    pub(crate) fn append_feedback_markdown(&self, entry: &str) -> Result<(), SpineError> {
        feedback::append_markdown_entry(&self.feedback_path(), entry)
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
