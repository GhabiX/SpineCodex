use crate::spine::SpineError;
use crate::spine::model::LoggedPressureEvent;
use serde::Serialize;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Read;
use std::io::Seek;
use std::io::Write;
use std::path::Path;

pub(super) fn append_json_line<T: Serialize>(path: &Path, value: &T) -> Result<(), SpineError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    if file.metadata()?.len() > 0 && last_byte(path)? != Some(b'\n') {
        file.write_all(b"\n")?;
    }
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n")?;
    Ok(())
}

fn last_byte(path: &Path) -> Result<Option<u8>, SpineError> {
    let mut file = File::open(path)?;
    if file.metadata()?.len() == 0 {
        return Ok(None);
    }
    file.seek(std::io::SeekFrom::End(-1))?;
    let mut byte = [0u8; 1];
    file.read_exact(&mut byte)?;
    Ok(Some(byte[0]))
}

pub(super) fn read_json_lines(path: &Path) -> Result<Vec<LoggedPressureEvent>, SpineError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(err) => {
            tracing::debug!(
                "skipping Spine pressure metadata: failed to open {}: {err}",
                path.display()
            );
            return Ok(Vec::new());
        }
    };
    let reader = BufReader::new(file);
    let mut out = Vec::new();
    for (line_index, line) in reader.lines().enumerate() {
        let line = match line {
            Ok(line) => line,
            Err(err) => {
                tracing::debug!(
                    "skipping Spine pressure metadata line {} in {}: {err}",
                    line_index + 1,
                    path.display()
                );
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str(&line) {
            Ok(event) => out.push(event),
            Err(err) => {
                tracing::debug!(
                    "skipping malformed Spine pressure metadata line {} in {}: {err}",
                    line_index + 1,
                    path.display()
                );
            }
        }
    }
    Ok(out)
}
