use crate::spine::SpineError;
use crate::spine::io::locator_path;
use crate::spine::io::read_json_file;
use crate::spine::io::rollout_parent;
use crate::spine::io::rollout_stem;
use serde::Deserialize;
use serde::Serialize;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

const LOCATOR_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Locator {
    version: u32,
    base: String,
}

pub(super) fn root_for_rollout(rollout_path: &Path) -> Result<PathBuf, SpineError> {
    let locator_path = locator_path(rollout_path)?;
    let locator: Locator = read_json_file(&locator_path)?;
    if locator.version != LOCATOR_VERSION {
        return Err(SpineError::InvalidStore(format!(
            "unsupported spine locator version {}",
            locator.version
        )));
    }
    Ok(rollout_parent(rollout_path)?.join(locator.base))
}

pub(super) fn has_for_rollout(rollout_path: &Path) -> Result<bool, SpineError> {
    Ok(locator_path(rollout_path)?.exists())
}

pub(super) fn sidecar_root_for_rollout(rollout_path: &Path) -> Result<PathBuf, SpineError> {
    let parent = rollout_parent(rollout_path)?;
    let stem = rollout_stem(rollout_path)?;
    Ok(parent.join(format!("spine-{stem}")))
}

pub(super) fn write_locator_for_root(rollout_path: &Path, root: &Path) -> Result<(), SpineError> {
    let content = locator_content_for_root(rollout_path, root)?;
    write_locator_content_atomically(&locator_path(rollout_path)?, &content)
}

fn write_new_locator_for_root(rollout_path: &Path, root: &Path) -> Result<bool, SpineError> {
    let content = locator_content_for_root(rollout_path, root)?;
    let locator_path = locator_path(rollout_path)?;
    let temp_path = write_locator_temp(&locator_path, &content)?;
    match std::fs::hard_link(&temp_path, &locator_path) {
        Ok(()) => {
            let _ = std::fs::remove_file(&temp_path);
            Ok(true)
        }
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            let _ = std::fs::remove_file(&temp_path);
            Ok(false)
        }
        Err(err) => {
            let _ = std::fs::remove_file(&temp_path);
            Err(err.into())
        }
    }
}

fn write_locator_content_atomically(locator_path: &Path, content: &str) -> Result<(), SpineError> {
    let temp_path = write_locator_temp(locator_path, content)?;
    if let Err(err) = std::fs::rename(&temp_path, locator_path) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(err.into());
    }
    Ok(())
}

fn write_locator_temp(locator_path: &Path, content: &str) -> Result<PathBuf, SpineError> {
    let parent = locator_path
        .parent()
        .ok_or_else(|| SpineError::InvalidStore("locator path has no parent".to_string()))?;
    let file_name = locator_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| SpineError::InvalidStore("invalid locator path".to_string()))?;
    std::fs::create_dir_all(parent)?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    for attempt in 0..1000u32 {
        let temp_path = parent.join(format!(
            ".{file_name}.tmp-{}-{nanos}-{attempt}",
            std::process::id()
        ));
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
        {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err.into()),
        };
        let write_result = file
            .write_all(content.as_bytes())
            .and_then(|()| file.sync_all());
        drop(file);
        if let Err(err) = write_result {
            let _ = std::fs::remove_file(&temp_path);
            return Err(err.into());
        }
        return Ok(temp_path);
    }
    Err(SpineError::InvalidStore(format!(
        "failed to allocate temp locator for {}",
        locator_path.display()
    )))
}

fn locator_content_for_root(rollout_path: &Path, root: &Path) -> Result<String, SpineError> {
    let locator = locator_for_root(rollout_path, root)?;
    Ok(serde_json::to_string_pretty(&locator)? + "\n")
}

fn locator_for_root(rollout_path: &Path, root: &Path) -> Result<Locator, SpineError> {
    let parent = rollout_parent(rollout_path)?;
    if root.parent() != Some(parent) {
        return Err(SpineError::InvalidStore(format!(
            "sidecar root {} is not under rollout parent {}",
            root.display(),
            parent.display()
        )));
    }
    Ok(Locator {
        version: LOCATOR_VERSION,
        base: root
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| SpineError::InvalidStore("invalid sidecar path".to_string()))?
            .to_string(),
    })
}

pub(super) fn create_unpublished_clone_root(rollout_path: &Path) -> Result<PathBuf, SpineError> {
    let parent = rollout_parent(rollout_path)?;
    let stem = rollout_stem(rollout_path)?;
    std::fs::create_dir_all(parent)?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    for attempt in 0..1000u32 {
        let root = parent.join(format!(
            ".spine-{stem}.clone-{}-{nanos}-{attempt}",
            std::process::id()
        ));
        match std::fs::create_dir(&root) {
            Ok(()) => return Ok(root),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err.into()),
        }
    }
    Err(SpineError::InvalidStore(format!(
        "failed to allocate unpublished sidecar for {}",
        rollout_path.display()
    )))
}

pub(super) fn publish_unpublished_clone(
    rollout_path: &Path,
    staging_root: &Path,
) -> Result<(), SpineError> {
    if has_for_rollout(rollout_path)? {
        discard_unpublished_sidecar(staging_root);
        return Ok(());
    }
    if !write_new_locator_for_root(rollout_path, staging_root)? {
        discard_unpublished_sidecar(staging_root);
    }
    Ok(())
}

pub(super) fn discard_unpublished_sidecar(root: &Path) {
    let _ = std::fs::remove_dir_all(root);
}
