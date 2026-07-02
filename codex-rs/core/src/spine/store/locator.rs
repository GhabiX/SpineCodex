use crate::spine::SpineError;
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
const SESSIONS_DIR: &str = "sessions";
const ARCHIVED_SESSIONS_DIR: &str = "archived_sessions";
const SPINE_SESSIONS_DIR: &str = "spine-session";
const LOCATOR_FILE_NAME: &str = "locator.json";

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Locator {
    version: u32,
    base: String,
}

pub(super) fn root_for_rollout(rollout_path: &Path) -> Result<PathBuf, SpineError> {
    let new_locator_path = locator_path(rollout_path)?;
    if new_locator_path.exists() {
        return read_root_from_locator(rollout_path, &new_locator_path);
    }
    read_root_from_locator(rollout_path, &legacy_locator_path(rollout_path)?)
}

pub(super) fn has_for_rollout(rollout_path: &Path) -> Result<bool, SpineError> {
    Ok(locator_path(rollout_path)?.exists() || legacy_locator_path(rollout_path)?.exists())
}

pub(super) fn sidecar_root_for_rollout(rollout_path: &Path) -> Result<PathBuf, SpineError> {
    Ok(layout_for_rollout(rollout_path)?.root)
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
    let layout = layout_for_rollout(rollout_path)?;
    let base = root
        .strip_prefix(&layout.codex_home)
        .map_err(|_| {
            SpineError::InvalidStore(format!(
                "sidecar root {} is not under codex home {}",
                root.display(),
                layout.codex_home.display()
            ))
        })?
        .to_path_buf();
    Ok(Locator {
        version: LOCATOR_VERSION,
        base: base
            .to_str()
            .ok_or_else(|| SpineError::InvalidStore("invalid sidecar path".to_string()))?
            .to_string(),
    })
}

pub(super) fn create_unpublished_clone_root(rollout_path: &Path) -> Result<PathBuf, SpineError> {
    let layout = layout_for_rollout(rollout_path)?;
    let parent = layout
        .root
        .parent()
        .ok_or_else(|| SpineError::InvalidStore("sidecar root has no parent".to_string()))?;
    std::fs::create_dir_all(parent)?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    for attempt in 0..1000u32 {
        let root = parent.join(format!(
            ".{}.clone-{}-{nanos}-{attempt}",
            layout
                .root
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| SpineError::InvalidStore("invalid sidecar path".to_string()))?,
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

pub(super) fn published_root_for_unpublished_clone(
    rollout_path: &Path,
    staging_root: &Path,
) -> Result<PathBuf, SpineError> {
    let layout = layout_for_rollout(rollout_path)?;
    if layout.locator_path == layout.root.join(LOCATOR_FILE_NAME) {
        Ok(layout.root)
    } else {
        Ok(staging_root.to_path_buf())
    }
}

pub(super) fn publish_unpublished_clone(
    rollout_path: &Path,
    staging_root: &Path,
) -> Result<(), SpineError> {
    if has_for_rollout(rollout_path)? {
        discard_unpublished_sidecar(staging_root);
        return Ok(());
    }
    let layout = layout_for_rollout(rollout_path)?;
    if layout.locator_path == layout.root.join(LOCATOR_FILE_NAME) {
        return publish_canonical_unpublished_clone(rollout_path, staging_root, &layout);
    }
    if !write_new_locator_for_root(rollout_path, staging_root)? {
        discard_unpublished_sidecar(staging_root);
    }
    Ok(())
}

pub(super) fn discard_unpublished_sidecar(root: &Path) {
    let _ = std::fs::remove_dir_all(root);
}

fn publish_canonical_unpublished_clone(
    rollout_path: &Path,
    staging_root: &Path,
    layout: &RolloutSidecarLayout,
) -> Result<(), SpineError> {
    if layout.root.exists() {
        discard_unpublished_sidecar(staging_root);
        return write_locator_for_root(rollout_path, &layout.root);
    }
    let content = locator_content_for_root(rollout_path, &layout.root)?;
    write_locator_content_atomically(&staging_root.join(LOCATOR_FILE_NAME), &content)?;
    match std::fs::rename(staging_root, &layout.root) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            discard_unpublished_sidecar(staging_root);
            write_locator_for_root(rollout_path, &layout.root)
        }
        Err(err) => Err(SpineError::InvalidStore(format!(
            "failed to publish Spine sidecar {} to {}: {err}",
            staging_root.display(),
            layout.root.display()
        ))),
    }
}

fn read_root_from_locator(rollout_path: &Path, locator_path: &Path) -> Result<PathBuf, SpineError> {
    let locator: Locator = read_json_file(locator_path)?;
    if locator.version != LOCATOR_VERSION {
        return Err(SpineError::InvalidStore(format!(
            "unsupported spine locator version {}",
            locator.version
        )));
    }
    let base = Path::new(&locator.base);
    if base.is_absolute() {
        return Ok(base.to_path_buf());
    }
    let layout = layout_for_rollout(rollout_path)?;
    let new_root = layout.codex_home.join(base);
    if new_root.exists() || locator_path == layout.locator_path {
        return Ok(new_root);
    }
    Ok(rollout_parent(rollout_path)?.join(base))
}

fn locator_path(rollout_path: &Path) -> Result<PathBuf, SpineError> {
    Ok(layout_for_rollout(rollout_path)?.locator_path)
}

fn legacy_locator_path(rollout_path: &Path) -> Result<PathBuf, SpineError> {
    Ok(rollout_parent(rollout_path)?.join(format!("{}.spine.json", rollout_stem(rollout_path)?)))
}

struct RolloutSidecarLayout {
    codex_home: PathBuf,
    root: PathBuf,
    locator_path: PathBuf,
}

fn layout_for_rollout(rollout_path: &Path) -> Result<RolloutSidecarLayout, SpineError> {
    if let Some((codex_home, year, month, day)) = canonical_rollout_date_parts(rollout_path) {
        let sidecar_name = sidecar_name_for_rollout(rollout_path)?;
        let root = codex_home
            .join(SPINE_SESSIONS_DIR)
            .join(year)
            .join(month)
            .join(day)
            .join(sidecar_name);
        let locator_path = root.join(LOCATOR_FILE_NAME);
        return Ok(RolloutSidecarLayout {
            codex_home,
            root,
            locator_path,
        });
    }

    let parent = rollout_parent(rollout_path)?.to_path_buf();
    let stem = rollout_stem(rollout_path)?;
    let root = parent.join(format!("spine-{stem}"));
    let locator_path = parent.join(format!("{stem}.spine.json"));
    Ok(RolloutSidecarLayout {
        codex_home: parent,
        root,
        locator_path,
    })
}

fn canonical_rollout_date_parts(rollout_path: &Path) -> Option<(PathBuf, String, String, String)> {
    let day = rollout_path.parent()?;
    let month = day.parent()?;
    let year = month.parent()?;
    let sessions_dir = year.parent()?;
    let dir_name = sessions_dir.file_name()?.to_str()?;
    if dir_name != SESSIONS_DIR && dir_name != ARCHIVED_SESSIONS_DIR {
        return None;
    }
    Some((
        sessions_dir.parent()?.to_path_buf(),
        year.file_name()?.to_str()?.to_string(),
        month.file_name()?.to_str()?.to_string(),
        day.file_name()?.to_str()?.to_string(),
    ))
}

fn sidecar_name_for_rollout(rollout_path: &Path) -> Result<String, SpineError> {
    let stem = rollout_stem(rollout_path)?;
    let suffix = stem.strip_prefix("rollout-").ok_or_else(|| {
        SpineError::InvalidStore(format!(
            "rollout path {} has unexpected stem {stem}",
            rollout_path.display()
        ))
    })?;
    Ok(format!("sidecar-{suffix}"))
}
