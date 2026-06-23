use codex_protocol::models::ResponseItem;
use std::path::Path;

use super::SpineError;
use super::SpineRuntime;
use crate::spine::checkpoint::SpineCheckpoint;

impl SpineRuntime {
    pub(crate) fn checkpoint_before_user_msg(
        &self,
        rollout_path: &Path,
        raw_ordinal: u64,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<(), SpineError> {
        let raw_end = usize::try_from(raw_ordinal)
            .map_err(|_| SpineError::InvalidEvent("raw ordinal overflow".to_string()))?;
        let prefix = raw_items.get(..raw_end).ok_or_else(|| {
            SpineError::InvalidEvent("checkpoint raw ordinal outside raw history".to_string())
        })?;
        let checkpoint = self.build_runtime_checkpoint(rollout_path, raw_ordinal, prefix)?;
        self.store.write_checkpoint(&checkpoint)
    }

    pub(crate) fn checkpoint_initial(
        &self,
        rollout_path: &Path,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<(), SpineError> {
        let mut checkpoint = self.build_runtime_checkpoint(rollout_path, 0, raw_items)?;
        checkpoint.checkpoint_id = "initial".to_string();
        self.store.write_initial_checkpoint(&checkpoint)
    }

    fn build_runtime_checkpoint(
        &self,
        rollout_path: &Path,
        raw_ordinal: u64,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<SpineCheckpoint, SpineError> {
        self.ensure_jit_enabled("Spine checkpoint")?;
        let trim_projection = self.current_trim_projection()?;
        self.parser.build_checkpoint(
            rollout_path,
            raw_ordinal,
            self.ledger.next_event_seq,
            self.pressure_seq_watermark()?,
            self.trim_seq_watermark()?,
            &self.raw_live,
            raw_items,
            &trim_projection,
        )
    }
}
