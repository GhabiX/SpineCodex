use super::SpineStore;
use crate::spine::SpineError;
use crate::spine::checkpoint::SpineCheckpoint;
use crate::spine::io::read_json_file;
#[cfg(test)]
use crate::spine::io::write_json_file;
use crate::spine::io::write_json_file_if_unchanged;

impl SpineStore {
    pub(in crate::spine) fn checkpoint_for_raw_ordinal(
        &self,
        raw_ordinal: u64,
    ) -> Result<SpineCheckpoint, SpineError> {
        read_json_file(&self.checkpoint_path(raw_ordinal))
    }

    #[cfg(test)]
    pub(in crate::spine) fn checkpoint_for_test(
        &self,
        raw_ordinal: u64,
    ) -> Result<SpineCheckpoint, SpineError> {
        self.checkpoint_for_raw_ordinal(raw_ordinal)
    }

    #[cfg(test)]
    pub(in crate::spine) fn initial_checkpoint_for_test(
        &self,
    ) -> Result<SpineCheckpoint, SpineError> {
        read_json_file(&self.initial_checkpoint_path())
    }

    #[cfg(test)]
    pub(crate) fn initial_checkpoint_identity_for_test(
        &self,
    ) -> Result<(String, String), SpineError> {
        let checkpoint: SpineCheckpoint = read_json_file(&self.initial_checkpoint_path())?;
        Ok((checkpoint.checkpoint_id, checkpoint.cursor))
    }

    pub(in crate::spine) fn checkpoints(&self) -> Result<Vec<SpineCheckpoint>, SpineError> {
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

    pub(in crate::spine) fn write_checkpoint(
        &self,
        checkpoint: &SpineCheckpoint,
    ) -> Result<(), SpineError> {
        let path = self.checkpoint_path(checkpoint.raw_ordinal);
        write_json_file_if_unchanged(&path, checkpoint)
    }

    pub(in crate::spine) fn write_initial_checkpoint(
        &self,
        checkpoint: &SpineCheckpoint,
    ) -> Result<(), SpineError> {
        write_json_file_if_unchanged(&self.initial_checkpoint_path(), checkpoint)
    }

    pub(in crate::spine) fn rollback_checkpoint(
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

    pub(in crate::spine) fn resume_checkpoint(
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
}
