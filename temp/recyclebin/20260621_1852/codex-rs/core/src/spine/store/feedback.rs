use crate::spine::SpineError;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

pub(super) fn append_markdown_entry(path: &Path, entry: &str) -> Result<(), SpineError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    if file.metadata()?.len() > 0 {
        file.write_all(b"\n")?;
    }
    file.write_all(entry.as_bytes())?;
    file.write_all(b"\n")?;
    Ok(())
}
