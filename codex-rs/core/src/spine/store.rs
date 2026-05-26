use crate::spine::SpineError;
use crate::spine::checkpoint::SpineCheckpoint;
use crate::spine::io::append_json_line;
use crate::spine::io::locator_path;
use crate::spine::io::read_json_file;
use crate::spine::io::read_json_lines;
use crate::spine::io::rollout_parent;
use crate::spine::io::rollout_stem;
use crate::spine::io::sha1_hex;
use crate::spine::io::write_json_file;
use crate::spine::io::write_json_file_if_unchanged;
use crate::spine::model::KEvent;
use crate::spine::model::LoggedKEvent;
use crate::spine::model::MemRecord;
use crate::spine::model::RawMask;
use serde::Deserialize;
use serde::Serialize;
use std::path::Path;
use std::path::PathBuf;

const LOCATOR_VERSION: u32 = 1;
const TREE_FILE: &str = "tree.jsonl";
const MEM_FILE: &str = "mem.jsonl";
const CHECKPOINT_DIR: &str = "checkpoints";
const INITIAL_CHECKPOINT_FILE: &str = "initial.json";

pub(super) const BODY_DIR: &str = "memory";

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Locator {
    version: u32,
    base: String,
}

#[derive(Clone, Debug)]
pub(crate) struct SpineStore {
    pub(super) root: PathBuf,
}

impl SpineStore {
    pub(crate) fn for_rollout(rollout_path: &Path) -> Result<Self, SpineError> {
        let locator_path = locator_path(rollout_path)?;
        let locator: Locator = read_json_file(&locator_path)?;
        if locator.version != LOCATOR_VERSION {
            return Err(SpineError::InvalidStore(format!(
                "unsupported spine locator version {}",
                locator.version
            )));
        }
        Ok(Self {
            root: rollout_parent(rollout_path)?.join(locator.base),
        })
    }

    pub(crate) fn create_for_rollout(rollout_path: &Path) -> Result<Self, SpineError> {
        let parent = rollout_parent(rollout_path)?;
        let stem = rollout_stem(rollout_path)?;
        let root = parent.join(format!("spine-{stem}"));
        std::fs::create_dir_all(&root)?;
        let locator = Locator {
            version: LOCATOR_VERSION,
            base: root
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| SpineError::InvalidStore("invalid sidecar path".to_string()))?
                .to_string(),
        };
        write_json_file(&locator_path(rollout_path)?, &locator)?;
        Ok(Self { root })
    }

    pub(crate) fn clone_for_rollout_with_raw_live(
        source_rollout_path: &Path,
        target_rollout_path: &Path,
        raw_live: &[bool],
    ) -> Result<(), SpineError> {
        if !Self::has_for_rollout(source_rollout_path)? {
            return Ok(());
        }
        if Self::has_for_rollout(target_rollout_path)? {
            return Ok(());
        }
        let source = Self::for_rollout(source_rollout_path)?;
        let target = Self::create_for_rollout(target_rollout_path)?;
        let mask = RawMask::new(raw_live);
        for event in source.events()? {
            if event.allowed_by(mask)? {
                target.append_event(&event.event)?;
            }
        }
        for mem in source.mems()? {
            if mem.allowed_by(mask)? {
                let body = source.read_memory_body(&mem)?;
                let body_path = target.write_memory_body(&mem.compact_id, &body)?;
                let cloned = MemRecord { body_path, ..mem };
                target.append_mem(&cloned)?;
            }
        }
        Ok(())
    }

    pub(crate) fn has_for_rollout(rollout_path: &Path) -> Result<bool, SpineError> {
        Ok(locator_path(rollout_path)?.exists())
    }

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

    pub(super) fn append_event(&self, event: &KEvent) -> Result<u64, SpineError> {
        let seq = self.next_event_seq()?;
        append_json_line(
            &self.tree_path(),
            &LoggedKEvent {
                seq,
                event: event.clone(),
            },
        )?;
        Ok(seq)
    }

    pub(super) fn append_mem(&self, mem: &MemRecord) -> Result<(), SpineError> {
        append_json_line(&self.mem_path(), mem)
    }

    pub(super) fn events(&self) -> Result<Vec<LoggedKEvent>, SpineError> {
        read_json_lines(&self.tree_path())
    }

    #[cfg(test)]
    pub(super) fn events_for_test(&self) -> Result<Vec<LoggedKEvent>, SpineError> {
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
    pub(super) fn checkpoint_for_test(
        &self,
        raw_ordinal: u64,
    ) -> Result<SpineCheckpoint, SpineError> {
        read_json_file(&self.checkpoint_path(raw_ordinal))
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
            .last()
            .map(|event| event.seq + 1)
            .unwrap_or(0))
    }

    pub(super) fn mems(&self) -> Result<Vec<MemRecord>, SpineError> {
        if !self.mem_path().exists() {
            return Ok(Vec::new());
        }
        read_json_lines(&self.mem_path())
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

    pub(super) fn write_memory_body(
        &self,
        compact_id: &str,
        body: &str,
    ) -> Result<String, SpineError> {
        let dir = self.root.join(BODY_DIR);
        std::fs::create_dir_all(&dir)?;
        let rel = format!("{BODY_DIR}/{compact_id}.md");
        std::fs::write(self.root.join(&rel), body)?;
        Ok(rel)
    }

    pub(super) fn read_memory_body(&self, mem: &MemRecord) -> Result<String, SpineError> {
        let body = std::fs::read_to_string(self.root.join(&mem.body_path))?;
        if sha1_hex(body.as_bytes()) != mem.body_hash {
            return Err(SpineError::InvalidStore(format!(
                "memory body hash mismatch for {}",
                mem.compact_id
            )));
        }
        Ok(body)
    }
}
