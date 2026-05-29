use crate::spine::SpineError;
use crate::spine::checkpoint::CheckpointMemoryRef;
use crate::spine::checkpoint::SpineCheckpoint;
use crate::spine::compact_checkpoint::SpineCompactCheckpoint;
use crate::spine::compact_checkpoint::compact_checkpoint_replacement_history_hash;
use crate::spine::compact_checkpoint::validate_compact_checkpoint;
use crate::spine::io::append_json_line;
use crate::spine::io::hash_raw_live;
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
use crate::spine::model::LoggedPressureEvent;
use crate::spine::model::MemRecord;
use crate::spine::model::PressureEvent;
use crate::spine::model::RawMask;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Read;
use std::io::Seek;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

const LOCATOR_VERSION: u32 = 1;
const TREE_FILE: &str = "tree.jsonl";
const PRESSURE_FILE: &str = "pressure.jsonl";
const MEM_FILE: &str = "mem.jsonl";
const COMPACT_CHECKPOINT_FILE: &str = "compact_checkpoints.jsonl";
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
                target.append_logged_event(&event)?;
            }
        }
        for pressure in source.pressure_events()? {
            if pressure.allowed_by(raw_live) {
                target.append_logged_pressure_event(&pressure)?;
            }
        }
        let mut cloned_memory_paths = BTreeMap::new();
        for mem in source.mems()? {
            if mem.allowed_by(mask)? {
                let body = source.read_memory_body(&mem)?;
                let body_path = target.write_memory_body(&mem.compact_id, &body)?;
                cloned_memory_paths.insert(mem.compact_id.clone(), body_path.clone());
                let cloned = MemRecord { body_path, ..mem };
                target.append_mem(&cloned)?;
            }
        }
        for checkpoint in source.compact_checkpoints()? {
            let boundary = usize::try_from(checkpoint.raw_boundary).map_err(|_| {
                SpineError::InvalidEvent("compact checkpoint raw boundary overflow".to_string())
            })?;
            if boundary <= raw_live.len()
                && checkpoint.raw_live_hash == hash_raw_live(&raw_live[..boundary])
            {
                let checkpoint = clone_compact_checkpoint_for_target(
                    checkpoint,
                    target_rollout_path,
                    &cloned_memory_paths,
                )?;
                target.append_compact_checkpoint(&checkpoint)?;
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

    pub(super) fn pressure_path(&self) -> PathBuf {
        self.root.join(PRESSURE_FILE)
    }

    #[cfg(test)]
    pub(crate) fn pressure_path_for_test(&self) -> PathBuf {
        self.pressure_path()
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

    pub(super) fn append_event(&self, event: &KEvent) -> Result<u64, SpineError> {
        let seq = self.next_event_seq()?;
        self.append_logged_event(&LoggedKEvent {
            seq,
            event: event.clone(),
        })?;
        Ok(seq)
    }

    fn append_logged_event(&self, event: &LoggedKEvent) -> Result<(), SpineError> {
        append_json_line(&self.tree_path(), event)
    }

    pub(super) fn append_pressure_event(&self, event: &PressureEvent) -> Result<u64, SpineError> {
        let pressure_seq = self.next_pressure_seq()?;
        self.append_logged_pressure_event(&LoggedPressureEvent {
            pressure_seq,
            event: event.clone(),
        })?;
        Ok(pressure_seq)
    }

    fn append_logged_pressure_event(&self, event: &LoggedPressureEvent) -> Result<(), SpineError> {
        append_pressure_json_line(&self.pressure_path(), event)
    }

    pub(super) fn append_mem(&self, mem: &MemRecord) -> Result<(), SpineError> {
        append_json_line(&self.mem_path(), mem)
    }

    pub(super) fn append_compact_checkpoint(
        &self,
        checkpoint: &SpineCompactCheckpoint,
    ) -> Result<(), SpineError> {
        append_json_line(&self.compact_checkpoint_path(), checkpoint)
    }

    pub(super) fn events(&self) -> Result<Vec<LoggedKEvent>, SpineError> {
        read_json_lines(&self.tree_path())
    }

    pub(super) fn pressure_events(&self) -> Result<Vec<LoggedPressureEvent>, SpineError> {
        if !self.pressure_path().exists() {
            return Ok(Vec::new());
        }
        read_pressure_json_lines(&self.pressure_path())
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

    pub(super) fn mems(&self) -> Result<Vec<MemRecord>, SpineError> {
        if !self.mem_path().exists() {
            return Ok(Vec::new());
        }
        read_json_lines(&self.mem_path())
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
        raw_boundary: u64,
        replacement_history: &[codex_protocol::models::ResponseItem],
    ) -> Result<(), SpineError> {
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
        let mut checkpoints = checkpoints
            .into_iter()
            .filter(|checkpoint| {
                checkpoint.replacement_history_hash == replacement_history_hash
                    && checkpoint.h_ps_hash == replacement_history_hash
            })
            .collect::<Vec<_>>();
        if checkpoints.is_empty() {
            return Err(SpineError::InvalidStore(format!(
                "spine_task_tree replacement_history does not match sidecar h(PS) compact checkpoint at raw boundary {raw_boundary}"
            )));
        }
        checkpoints.sort_by_key(|checkpoint| checkpoint.token_seq);
        let mut last_err = None;
        for checkpoint in checkpoints.into_iter().rev() {
            match validate_compact_checkpoint(
                &checkpoint,
                rollout_path,
                raw_live,
                replacement_history,
            ) {
                Ok(()) => {
                    for memory in &checkpoint.memory_refs {
                        let body = std::fs::read_to_string(self.root.join(&memory.body_path))?;
                        if sha1_hex(body.as_bytes()) != memory.body_hash {
                            return Err(SpineError::InvalidStore(format!(
                                "memory body hash mismatch for {}",
                                memory.compact_id
                            )));
                        }
                    }
                    return Ok(());
                }
                Err(err) => {
                    last_err = Some(err);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| {
            SpineError::InvalidStore(format!(
                "missing spine compact checkpoint at raw boundary {raw_boundary}"
            ))
        }))
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

fn append_pressure_json_line<T: Serialize>(path: &Path, value: &T) -> Result<(), SpineError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    if file.metadata()?.len() > 0 && last_byte(path)? != Some(b'\n') {
        file.write_all(b"\n")?;
    }
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n")?;
    Ok(())
}

fn last_byte(path: &Path) -> Result<Option<u8>, SpineError> {
    let mut file = File::open(path)?;
    if file.metadata()?.len() == 0 {
        return Ok(None);
    }
    file.seek(std::io::SeekFrom::End(-1))?;
    let mut byte = [0u8; 1];
    file.read_exact(&mut byte)?;
    Ok(Some(byte[0]))
}

fn read_pressure_json_lines(path: &Path) -> Result<Vec<LoggedPressureEvent>, SpineError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(err) => {
            tracing::debug!(
                "skipping Spine pressure metadata: failed to open {}: {err}",
                path.display()
            );
            return Ok(Vec::new());
        }
    };
    let reader = BufReader::new(file);
    let mut out = Vec::new();
    for (line_index, line) in reader.lines().enumerate() {
        let line = match line {
            Ok(line) => line,
            Err(err) => {
                tracing::debug!(
                    "skipping Spine pressure metadata line {} in {}: {err}",
                    line_index + 1,
                    path.display()
                );
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str(&line) {
            Ok(event) => out.push(event),
            Err(err) => {
                tracing::debug!(
                    "skipping malformed Spine pressure metadata line {} in {}: {err}",
                    line_index + 1,
                    path.display()
                );
            }
        }
    }
    Ok(out)
}

fn clone_compact_checkpoint_for_target(
    checkpoint: SpineCompactCheckpoint,
    target_rollout_path: &Path,
    cloned_memory_paths: &BTreeMap<String, String>,
) -> Result<SpineCompactCheckpoint, SpineError> {
    let mut memory_refs = Vec::with_capacity(checkpoint.memory_refs.len());
    for memory in checkpoint.memory_refs {
        let body_path = cloned_memory_paths
            .get(&memory.compact_id)
            .ok_or_else(|| {
                SpineError::InvalidStore(format!(
                    "compact checkpoint references uncloned memory {}",
                    memory.compact_id
                ))
            })?
            .clone();
        memory_refs.push(CheckpointMemoryRef {
            body_path,
            ..memory
        });
    }
    Ok(SpineCompactCheckpoint {
        rollout_path: target_rollout_path.display().to_string(),
        memory_refs,
        ..checkpoint
    })
}
