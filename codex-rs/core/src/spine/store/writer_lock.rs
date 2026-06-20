use crate::spine::SpineError;
use std::fs::File;
use std::fs::OpenOptions;
use std::path::Path;

const WRITER_LOCK_FILE: &str = ".writer.lock";

pub(super) fn acquire(root: &Path) -> Result<File, SpineError> {
    std::fs::create_dir_all(root)?;
    let lock_path = root.join(WRITER_LOCK_FILE);
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(&lock_path)?;
    match lock.try_lock() {
        Ok(()) => Ok(lock),
        Err(std::fs::TryLockError::WouldBlock) => Err(SpineError::InvalidStore(format!(
            "Spine sidecar {} is already owned by another live Codex process",
            root.display()
        ))),
        Err(std::fs::TryLockError::Error(err)) => Err(err.into()),
    }
}
