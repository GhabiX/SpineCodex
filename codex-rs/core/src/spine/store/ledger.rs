use super::SpineStore;
use crate::spine::SpineError;
use crate::spine::compact_checkpoint::SpineCompactCheckpoint;
use crate::spine::io::append_json_line;
use crate::spine::io::read_json_lines;
use crate::spine::model::LoggedPressureEvent;
use crate::spine::model::LoggedSpineLedgerEvent;
use crate::spine::model::LoggedTrimEvent;
use crate::spine::model::MemRecord;
use crate::spine::model::MemoryContextAccountingRecord;
use crate::spine::model::MemoryContextAccountingWitnessRecord;
#[cfg(test)]
use crate::spine::model::PressureEvent;
use crate::spine::model::SpineCommitMarker;
use crate::spine::model::SpineLedgerEvent;

impl SpineStore {
    pub(in crate::spine) fn append_event(
        &self,
        event: &SpineLedgerEvent,
    ) -> Result<u64, SpineError> {
        let seq = self.next_event_seq()?;
        self.append_logged_event(&LoggedSpineLedgerEvent {
            seq,
            event: event.clone(),
        })?;
        Ok(seq)
    }

    pub(in crate::spine) fn append_logged_event(
        &self,
        event: &LoggedSpineLedgerEvent,
    ) -> Result<(), SpineError> {
        append_json_line(&self.tree_path(), event)
    }

    #[cfg(test)]
    pub(in crate::spine) fn append_pressure_event(
        &self,
        event: &PressureEvent,
    ) -> Result<u64, SpineError> {
        let pressure_seq = self.next_pressure_seq()?;
        self.append_logged_pressure_event(&LoggedPressureEvent {
            pressure_seq,
            event: event.clone(),
        })?;
        Ok(pressure_seq)
    }

    pub(in crate::spine) fn append_logged_pressure_event(
        &self,
        event: &LoggedPressureEvent,
    ) -> Result<(), SpineError> {
        super::pressure::append_json_line(&self.pressure_path(), event)
    }

    pub(in crate::spine) fn append_logged_trim_event(
        &self,
        event: &LoggedTrimEvent,
    ) -> Result<(), SpineError> {
        append_json_line(&self.trim_path(), event)
    }

    pub(in crate::spine) fn append_mem(&self, mem: &MemRecord) -> Result<(), SpineError> {
        append_json_line(&self.mem_path(), mem)
    }

    pub(in crate::spine) fn append_mem_accounting(
        &self,
        accounting: &MemoryContextAccountingRecord,
    ) -> Result<(), SpineError> {
        append_json_line(&self.mem_accounting_path(), accounting)
    }

    pub(in crate::spine) fn append_mem_accounting_witness(
        &self,
        witness: &MemoryContextAccountingWitnessRecord,
    ) -> Result<(), SpineError> {
        append_json_line(&self.mem_accounting_witness_path(), witness)
    }

    pub(in crate::spine) fn append_commit_marker(
        &self,
        marker: &SpineCommitMarker,
    ) -> Result<(), SpineError> {
        append_json_line(&self.commit_path(), marker)
    }

    pub(in crate::spine) fn append_compact_checkpoint(
        &self,
        checkpoint: &SpineCompactCheckpoint,
    ) -> Result<(), SpineError> {
        append_json_line(&self.compact_checkpoint_path(), checkpoint)
    }

    pub(in crate::spine) fn events(&self) -> Result<Vec<LoggedSpineLedgerEvent>, SpineError> {
        read_json_lines(&self.tree_path())
    }

    pub(in crate::spine) fn pressure_events(&self) -> Result<Vec<LoggedPressureEvent>, SpineError> {
        if !self.pressure_path().exists() {
            return Ok(Vec::new());
        }
        super::pressure::read_json_lines(&self.pressure_path())
    }

    pub(in crate::spine) fn trim_events(&self) -> Result<Vec<LoggedTrimEvent>, SpineError> {
        if !self.trim_path().exists() {
            return Err(SpineError::InvalidStore(format!(
                "missing required Spine trim ledger: {}",
                self.trim_path().display()
            )));
        }
        read_json_lines(&self.trim_path())
    }

    pub(in crate::spine) fn next_event_seq(&self) -> Result<u64, SpineError> {
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

    pub(in crate::spine) fn next_pressure_seq(&self) -> Result<u64, SpineError> {
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

    pub(in crate::spine) fn next_trim_seq(&self) -> Result<u64, SpineError> {
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

    pub(in crate::spine) fn mems(&self) -> Result<Vec<MemRecord>, SpineError> {
        if !self.mem_path().exists() {
            return Ok(Vec::new());
        }
        read_json_lines(&self.mem_path())
    }

    pub(in crate::spine) fn mem_accounting(
        &self,
    ) -> Result<Vec<MemoryContextAccountingRecord>, SpineError> {
        if !self.mem_accounting_path().exists() {
            return Ok(Vec::new());
        }
        read_json_lines(&self.mem_accounting_path())
    }

    pub(in crate::spine) fn mem_accounting_witnesses(
        &self,
    ) -> Result<Vec<MemoryContextAccountingWitnessRecord>, SpineError> {
        if !self.mem_accounting_witness_path().exists() {
            return Ok(Vec::new());
        }
        read_json_lines(&self.mem_accounting_witness_path())
    }

    pub(in crate::spine) fn commit_markers(&self) -> Result<Vec<SpineCommitMarker>, SpineError> {
        if !self.commit_path().exists() {
            return Ok(Vec::new());
        }
        read_json_lines(&self.commit_path())
    }

    pub(in crate::spine) fn compact_checkpoints(
        &self,
    ) -> Result<Vec<SpineCompactCheckpoint>, SpineError> {
        if !self.compact_checkpoint_path().exists() {
            return Ok(Vec::new());
        }
        read_json_lines(&self.compact_checkpoint_path())
    }
}
