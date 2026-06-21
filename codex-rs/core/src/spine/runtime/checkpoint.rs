use codex_protocol::models::ResponseItem;
use std::path::Path;

use super::SpineError;
use super::SpineRuntime;
use crate::spine::checkpoint::build_checkpoint;

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
        let context = self.materialize_history(prefix)?;
        let checkpoint = build_checkpoint(
            rollout_path,
            raw_ordinal,
            self.ledger.next_event_seq,
            self.pressure_seq_watermark()?,
            self.trim_seq_watermark()?,
            &self.raw_live,
            &self.parse_stack,
            &context,
        )?;
        self.store.write_checkpoint(&checkpoint)
    }

    pub(crate) fn checkpoint_initial(
        &self,
        rollout_path: &Path,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<(), SpineError> {
        let context = self.materialize_history(raw_items)?;
        let mut checkpoint = build_checkpoint(
            rollout_path,
            0,
            self.ledger.next_event_seq,
            self.pressure_seq_watermark()?,
            self.trim_seq_watermark()?,
            &self.raw_live,
            &self.parse_stack,
            &context,
        )?;
        checkpoint.checkpoint_id = "initial".to_string();
        self.store.write_initial_checkpoint(&checkpoint)
    }
}
