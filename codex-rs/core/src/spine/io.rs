use super::SpineError;
use codex_protocol::models::ResponseItem;
use serde::Deserialize;
use serde::Serialize;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

pub(super) fn locator_path(rollout_path: &Path) -> Result<PathBuf, SpineError> {
    Ok(rollout_parent(rollout_path)?.join(format!("{}.spine.json", rollout_stem(rollout_path)?)))
}

pub(super) fn rollout_parent(path: &Path) -> Result<&Path, SpineError> {
    path.parent()
        .ok_or_else(|| SpineError::InvalidStore("rollout path has no parent".to_string()))
}

pub(super) fn rollout_stem(path: &Path) -> Result<String, SpineError> {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(ToString::to_string)
        .ok_or_else(|| SpineError::InvalidStore("rollout path has no UTF-8 stem".to_string()))
}

pub(super) fn append_json_line<T: Serialize>(path: &Path, value: &T) -> Result<(), SpineError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n")?;
    Ok(())
}

pub(super) fn read_json_lines<T: for<'de> Deserialize<'de>>(
    path: &Path,
) -> Result<Vec<T>, SpineError> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut out = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        out.push(serde_json::from_str(&line)?);
    }
    Ok(out)
}

pub(super) fn read_json_file<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, SpineError> {
    let text = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&text)?)
}

#[cfg(test)]
pub(super) fn write_json_file<T: Serialize>(path: &Path, value: &T) -> Result<(), SpineError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(value)? + "\n")?;
    Ok(())
}

pub(super) fn write_json_file_if_unchanged<T: Serialize>(
    path: &Path,
    value: &T,
) -> Result<(), SpineError> {
    let content = serde_json::to_string_pretty(value)? + "\n";
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if path.exists() {
        let existing = std::fs::read_to_string(path)?;
        if existing == content {
            return Ok(());
        }
        return Err(SpineError::InvalidStore(format!(
            "checkpoint file {} already exists with different content",
            path.display()
        )));
    }
    std::fs::write(path, content)?;
    Ok(())
}

pub(super) fn sha1_hex(bytes: &[u8]) -> String {
    use sha1::Digest;
    let mut hasher = sha1::Sha1::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

pub(super) fn hash_raw_live(raw_live: &[bool]) -> String {
    use sha1::Digest;
    let mut hasher = sha1::Sha1::new();
    for live in raw_live {
        hasher.update(if *live { b"1" } else { b"0" });
    }
    format!("{:x}", hasher.finalize())
}

pub(super) fn hash_raw_live_prefix_all_true(len: usize) -> String {
    use sha1::Digest;
    let mut hasher = sha1::Sha1::new();
    for _ in 0..len {
        hasher.update(b"1");
    }
    format!("{:x}", hasher.finalize())
}

pub(super) fn hash_response_items(items: &[ResponseItem]) -> Result<String, SpineError> {
    let bytes = serde_json::to_vec(items)?;
    Ok(sha1_hex(&bytes))
}
