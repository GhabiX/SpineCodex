use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use std::fs;
use std::fs::OpenOptions;
use std::io::ErrorKind;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

const SUMMARY_FILENAME_CHAR_BUDGET: usize = 96;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpinetreeMemoryProjectionEntry {
    pub(crate) node_id: String,
    pub(crate) summary: String,
    pub(crate) body: String,
}

#[derive(Clone, Debug)]
pub(crate) struct SpinetreeMemoryProjection {
    root_dir: PathBuf,
}

impl SpinetreeMemoryProjection {
    pub(crate) fn from_config(
        cwd: &Path,
        session_id: &str,
        enabled: bool,
        spine_jit_enabled: bool,
    ) -> Result<Option<Self>> {
        if !enabled {
            return Ok(None);
        }
        if !spine_jit_enabled {
            bail!(
                "feature `spinetree_memory_projection` requires `spine_jit` because only the Spine tree reducer produces closed node memories"
            );
        }
        let session_dir_name = format!(
            "{}_{}",
            chrono::Local::now().format("%Y%m%d_%H%M%S"),
            session_id
        );
        Ok(Some(Self {
            root_dir: cwd.join(".codex").join("spinetree").join(session_dir_name),
        }))
    }

    pub(crate) fn persist(&self, entries: &[SpinetreeMemoryProjectionEntry]) -> Result<()> {
        for entry in entries {
            self.persist_entry(entry)?;
        }
        Ok(())
    }

    fn persist_entry(&self, entry: &SpinetreeMemoryProjectionEntry) -> Result<()> {
        let memory_dir = self.root_dir.join(".memory");
        fs::create_dir_all(&memory_dir).with_context(|| {
            format!(
                "failed to create Spine memory projection directory {}",
                memory_dir.display()
            )
        })?;

        let target_path = memory_dir.join(format!("{}.md", entry.node_id));
        persist_readonly_target(&target_path, &entry.body)?;

        let summary = sanitize_summary_for_filename(&entry.summary);
        let projection_path = self
            .root_dir
            .join(format!("{}_{}.md", entry.node_id, summary));
        persist_symlink(&target_path, &projection_path)?;
        Ok(())
    }
}

fn persist_readonly_target(path: &Path, body: &str) -> Result<()> {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => {
            file.write_all(body.as_bytes())
                .with_context(|| format!("failed to write {}", path.display()))?;
            file.sync_all()
                .with_context(|| format!("failed to sync {}", path.display()))?;
        }
        Err(err) if err.kind() == ErrorKind::AlreadyExists => {
            let existing = fs::read_to_string(path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            if existing != body {
                bail!(
                    "Spine memory projection target {} already exists with different content",
                    path.display()
                );
            }
        }
        Err(err) => {
            return Err(err).with_context(|| format!("failed to create {}", path.display()));
        }
    }

    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_readonly(true);
    fs::set_permissions(path, permissions)
        .with_context(|| format!("failed to mark {} readonly", path.display()))?;
    Ok(())
}

fn persist_symlink(target: &Path, link: &Path) -> Result<()> {
    match fs::symlink_metadata(link) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let existing_target = resolve_symlink_target(link, fs::read_link(link)?);
            if existing_target != target {
                bail!(
                    "Spine memory projection symlink {} points at {}, expected {}",
                    link.display(),
                    existing_target.display(),
                    target.display()
                );
            }
            Ok(())
        }
        Ok(_) => bail!(
            "Spine memory projection path already exists and is not a symlink: {}",
            link.display()
        ),
        Err(err) if err.kind() == ErrorKind::NotFound => create_file_symlink(target, link)
            .with_context(|| {
                format!(
                    "failed to link Spine memory projection {} to {}",
                    link.display(),
                    target.display()
                )
            }),
        Err(err) => Err(err.into()),
    }
}

fn resolve_symlink_target(link_path: &Path, target: PathBuf) -> PathBuf {
    if target.is_absolute() {
        return target;
    }
    link_path
        .parent()
        .map(|parent| parent.join(&target))
        .unwrap_or(target)
}

#[cfg(unix)]
fn create_file_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn create_file_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(target, link)
}

fn sanitize_summary_for_filename(summary: &str) -> String {
    let mut sanitized = String::new();
    let mut last_was_separator = false;
    for ch in summary.trim().chars() {
        let ch = if ch.is_control()
            || matches!(
                ch,
                '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0'
            )
            || ch.is_whitespace()
        {
            '_'
        } else {
            ch
        };
        if ch == '_' {
            if last_was_separator {
                continue;
            }
            last_was_separator = true;
        } else {
            last_was_separator = false;
        }
        sanitized.push(ch);
        if sanitized.chars().count() >= SUMMARY_FILENAME_CHAR_BUDGET {
            break;
        }
    }
    let sanitized = sanitized.trim_matches('_').to_string();
    if sanitized.is_empty() {
        "node".to_string()
    } else {
        sanitized
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(body: &str) -> SpinetreeMemoryProjectionEntry {
        SpinetreeMemoryProjectionEntry {
            node_id: "1.2".to_string(),
            summary: "child task / close: memory?".to_string(),
            body: body.to_string(),
        }
    }

    #[test]
    fn requires_spine_jit_when_enabled() {
        let temp = tempfile::tempdir().unwrap();
        let err = SpinetreeMemoryProjection::from_config(temp.path(), "session", true, false)
            .unwrap_err();
        assert!(err.to_string().contains("requires `spine_jit`"));
        assert!(
            SpinetreeMemoryProjection::from_config(temp.path(), "session", false, false)
                .unwrap()
                .is_none()
        );
        assert!(!temp.path().join(".codex").exists());
    }

    #[test]
    fn persists_readonly_memory_and_symlink_idempotently() {
        let temp = tempfile::tempdir().unwrap();
        let projection = SpinetreeMemoryProjection {
            root_dir: temp.path().join(".codex/spinetree/test-session"),
        };

        projection.persist(&[entry("memory body")]).unwrap();
        projection.persist(&[entry("memory body")]).unwrap();

        let target = projection.root_dir.join(".memory/1.2.md");
        let link = projection.root_dir.join("1.2_child_task_close_memory.md");
        assert_eq!(fs::read_to_string(&link).unwrap(), "memory body");
        assert!(fs::metadata(&target).unwrap().permissions().readonly());
        assert!(fs::symlink_metadata(link).unwrap().file_type().is_symlink());
    }

    #[test]
    fn rejects_existing_different_memory() {
        let temp = tempfile::tempdir().unwrap();
        let projection = SpinetreeMemoryProjection {
            root_dir: temp.path().join(".codex/spinetree/test-session"),
        };
        projection.persist(&[entry("first")]).unwrap();

        let err = projection.persist(&[entry("second")]).unwrap_err();
        assert!(err.to_string().contains("different content"));
    }
}
