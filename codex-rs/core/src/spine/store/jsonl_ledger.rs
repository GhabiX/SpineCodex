use super::SpineStoreError;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Write;
use std::marker::PhantomData;
use std::path::Path;

pub(super) trait SequencedLedgerEvent {
    fn seq(&self) -> u64;
    fn set_seq(&mut self, seq: u64);
}

pub(super) struct JsonlLedger<'a, E> {
    path: &'a Path,
    label: &'static str,
    allow_missing: bool,
    _event: PhantomData<E>,
}

impl<'a, E> JsonlLedger<'a, E> {
    pub(super) fn required(path: &'a Path, label: &'static str) -> Self {
        Self {
            path,
            label,
            allow_missing: false,
            _event: PhantomData,
        }
    }

    pub(super) fn optional(path: &'a Path, label: &'static str) -> Self {
        Self {
            path,
            label,
            allow_missing: true,
            _event: PhantomData,
        }
    }

    pub(super) fn ensure_exists(&self) -> Result<(), SpineStoreError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| SpineStoreError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.path)
            .map_err(|source| SpineStoreError::Io {
                path: self.path.to_path_buf(),
                source,
            })?;
        Ok(())
    }
}

impl<E> JsonlLedger<'_, E> {
    pub(super) fn next_seq(&self) -> Result<u64, SpineStoreError> {
        if self.allow_missing && !self.path.exists() {
            return Ok(1);
        }

        let file = File::open(self.path).map_err(|source| SpineStoreError::Io {
            path: self.path.to_path_buf(),
            source,
        })?;
        let reader = BufReader::new(file);
        let mut count = 0_u64;
        for (index, line) in reader.lines().enumerate() {
            let line = line.map_err(|source| SpineStoreError::Io {
                path: self.path.to_path_buf(),
                source,
            })?;
            if line.trim().is_empty() {
                return Err(SpineStoreError::InvalidLedger(format!(
                    "{} line {} is empty",
                    self.label,
                    index + 1
                )));
            }
            count = count.checked_add(1).ok_or_else(|| {
                SpineStoreError::InvalidLedger(format!("{} has too many events", self.label))
            })?;
        }
        count.checked_add(1).ok_or_else(|| {
            SpineStoreError::InvalidLedger(format!("{} has too many events", self.label))
        })
    }
}

impl<E> JsonlLedger<'_, E>
where
    E: DeserializeOwned,
{
    pub(super) fn read(&self) -> Result<Vec<E>, SpineStoreError> {
        if self.allow_missing && !self.path.exists() {
            return Ok(Vec::new());
        }

        let file = File::open(self.path).map_err(|source| SpineStoreError::Io {
            path: self.path.to_path_buf(),
            source,
        })?;
        let reader = BufReader::new(file);
        let mut events = Vec::new();

        for (index, line) in reader.lines().enumerate() {
            let line = line.map_err(|source| SpineStoreError::Io {
                path: self.path.to_path_buf(),
                source,
            })?;
            if line.trim().is_empty() {
                return Err(SpineStoreError::InvalidLedger(format!(
                    "{} line {} is empty",
                    self.label,
                    index + 1
                )));
            }
            let event = serde_json::from_str(&line).map_err(|source| SpineStoreError::Json {
                path: self.path.to_path_buf(),
                source,
            })?;
            events.push(event);
        }

        Ok(events)
    }
}

impl<E> JsonlLedger<'_, E>
where
    E: Serialize,
{
    pub(super) fn append(&self, value: &E) -> Result<(), SpineStoreError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| SpineStoreError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.path)
            .map_err(|source| SpineStoreError::Io {
                path: self.path.to_path_buf(),
                source,
            })?;
        serde_json::to_writer(&mut file, value).map_err(|source| SpineStoreError::Json {
            path: self.path.to_path_buf(),
            source,
        })?;
        file.write_all(b"\n").map_err(|source| SpineStoreError::Io {
            path: self.path.to_path_buf(),
            source,
        })
    }

    pub(super) fn append_next_seq(
        &self,
        make: impl FnOnce(u64) -> E,
    ) -> Result<(), SpineStoreError> {
        let event = make(self.next_seq()?);
        self.append(&event)
    }
}

impl<E> JsonlLedger<'_, E>
where
    E: DeserializeOwned + SequencedLedgerEvent,
{
    pub(super) fn read_sequenced(&self) -> Result<Vec<E>, SpineStoreError> {
        let events = self.read()?;
        for (index, event) in events.iter().enumerate() {
            let expected_seq = u64::try_from(index + 1).map_err(|_| {
                SpineStoreError::InvalidLedger(format!("{} has too many events", self.label))
            })?;
            if event.seq() != expected_seq {
                return Err(SpineStoreError::InvalidLedger(format!(
                    "{} line {} has seq {}, expected {}",
                    self.label,
                    index + 1,
                    event.seq(),
                    expected_seq
                )));
            }
        }
        Ok(events)
    }
}

impl<E> JsonlLedger<'_, E>
where
    E: Serialize + SequencedLedgerEvent,
{
    pub(super) fn rewrite_resequenced(&self, events: Vec<E>) -> Result<(), SpineStoreError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| SpineStoreError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let mut file = File::create(self.path).map_err(|source| SpineStoreError::Io {
            path: self.path.to_path_buf(),
            source,
        })?;
        for (index, mut event) in events.into_iter().enumerate() {
            let seq = u64::try_from(index + 1).map_err(|_| {
                SpineStoreError::InvalidLedger(format!("{} has too many events", self.label))
            })?;
            event.set_seq(seq);
            serde_json::to_writer(&mut file, &event).map_err(|source| SpineStoreError::Json {
                path: self.path.to_path_buf(),
                source,
            })?;
            file.write_all(b"\n")
                .map_err(|source| SpineStoreError::Io {
                    path: self.path.to_path_buf(),
                    source,
                })?;
        }
        Ok(())
    }
}
