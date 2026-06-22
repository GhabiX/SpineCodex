use super::BODY_DIR;
use crate::spine::SpineError;
use crate::spine::io::sha1_hex;
use crate::spine::model::MemRecord;
use std::path::Path;

pub(super) fn write_body(
    store_root: &Path,
    compact_id: &str,
    body: &str,
) -> Result<String, SpineError> {
    let dir = store_root.join(BODY_DIR);
    std::fs::create_dir_all(&dir)?;
    let rel = format!("{BODY_DIR}/{compact_id}.md");
    let path = store_root.join(&rel);
    if path.exists() {
        let existing = std::fs::read_to_string(&path)?;
        if existing == body {
            return Ok(rel);
        }
        return Err(SpineError::InvalidStore(format!(
            "memory body {} already exists with different content",
            path.display()
        )));
    }
    std::fs::write(path, body)?;
    Ok(rel)
}

pub(super) fn read_body(store_root: &Path, mem: &MemRecord) -> Result<String, SpineError> {
    read_body_with_hash(
        store_root.join(&mem.body_path),
        &mem.compact_id,
        &mem.body_hash,
    )
}

pub(super) fn read_body_with_hash(
    path: impl AsRef<Path>,
    compact_id: &str,
    body_hash: &str,
) -> Result<String, SpineError> {
    let body = std::fs::read_to_string(path)?;
    if sha1_hex(body.as_bytes()) != body_hash {
        return Err(SpineError::InvalidStore(format!(
            "memory body hash mismatch for {}",
            compact_id
        )));
    }
    Ok(body)
}
