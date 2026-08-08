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
const USER_MESSAGES_FILENAME: &str = "USER.md";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpinetreeMemoryProjectionEntry {
    pub(crate) node_id: String,
    pub(crate) summary: String,
    pub(crate) body: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpinetreeUserMessageProjectionEntry {
    pub(crate) anchor: u64,
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

    pub(crate) fn persist(
        &self,
        entries: &[SpinetreeMemoryProjectionEntry],
        user_messages: &[SpinetreeUserMessageProjectionEntry],
    ) -> Result<()> {
        self.persist_user_messages(user_messages)?;
        for entry in entries {
            self.persist_entry(entry)?;
        }
        Ok(())
    }

    fn persist_entry(&self, entry: &SpinetreeMemoryProjectionEntry) -> Result<()> {
        fs::create_dir_all(&self.root_dir).with_context(|| {
            format!(
                "failed to create Spine memory projection directory {}",
                self.root_dir.display()
            )
        })?;

        let summary = sanitize_summary_for_filename(&entry.summary);
        let projection_path = self
            .root_dir
            .join(format!("{}_{}.md", entry.node_id, summary));
        persist_readonly_file(&projection_path, &entry.body)?;
        Ok(())
    }

    fn persist_user_messages(&self, entries: &[SpinetreeUserMessageProjectionEntry]) -> Result<()> {
        let projection_path = self.root_dir.join(USER_MESSAGES_FILENAME);
        let existing = match fs::symlink_metadata(&projection_path) {
            Ok(metadata) => {
                if !metadata.file_type().is_file() {
                    bail!(
                        "Spine User projection path already exists and is not a regular file: {}",
                        projection_path.display()
                    );
                }
                Some(
                    fs::read(&projection_path)
                        .with_context(|| format!("failed to read {}", projection_path.display()))?,
                )
            }
            Err(err) if err.kind() == ErrorKind::NotFound => None,
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("failed to inspect {}", projection_path.display()));
            }
        };
        if entries.is_empty() && existing.is_none() {
            return Ok(());
        }

        fs::create_dir_all(&self.root_dir).with_context(|| {
            format!(
                "failed to create Spine memory projection directory {}",
                self.root_dir.display()
            )
        })?;
        let body = render_user_messages(entries);
        if existing.as_deref() == Some(body.as_bytes()) {
            return Ok(());
        }
        if let Some(existing) = existing
            && body.as_bytes().starts_with(&existing)
        {
            return append_mutable_file(&projection_path, &body.as_bytes()[existing.len()..]);
        }
        replace_mutable_file(&projection_path, body.as_bytes())
    }
}

fn render_user_messages(entries: &[SpinetreeUserMessageProjectionEntry]) -> String {
    let mut blocks = vec!["# User Messages".to_string()];
    blocks.extend(
        entries
            .iter()
            .map(|entry| format!("## User Message [U{}]\n{}", entry.anchor, entry.body)),
    );
    format!("{}\n", blocks.join("\n\n"))
}

fn append_mutable_file(path: &Path, suffix: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .append(true)
        .open(path)
        .with_context(|| format!("failed to open {} for append", path.display()))?;
    file.write_all(suffix)
        .with_context(|| format!("failed to append {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to sync {}", path.display()))?;
    Ok(())
}

fn replace_mutable_file(path: &Path, body: &[u8]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true);
    if path.exists() {
        options.truncate(true);
    } else {
        options.create_new(true);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("failed to open {} for replacement", path.display()))?;
    file.write_all(body)
        .with_context(|| format!("failed to write {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to sync {}", path.display()))?;
    Ok(())
}

fn persist_readonly_file(path: &Path, body: &str) -> Result<()> {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => {
            file.write_all(body.as_bytes())
                .with_context(|| format!("failed to write {}", path.display()))?;
            file.sync_all()
                .with_context(|| format!("failed to sync {}", path.display()))?;
        }
        Err(err) if err.kind() == ErrorKind::AlreadyExists => {
            if !fs::symlink_metadata(path)?.file_type().is_file() {
                bail!(
                    "Spine memory projection path already exists and is not a regular file: {}",
                    path.display()
                );
            }
            let existing = fs::read_to_string(path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            if existing != body {
                bail!(
                    "Spine memory projection file {} already exists with different content",
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
    use pretty_assertions::assert_eq;

    fn entry(body: &str) -> SpinetreeMemoryProjectionEntry {
        SpinetreeMemoryProjectionEntry {
            node_id: "1.2".to_string(),
            summary: "child task / close: memory?".to_string(),
            body: body.to_string(),
        }
    }

    fn user_entry(anchor: u64, body: &str) -> SpinetreeUserMessageProjectionEntry {
        SpinetreeUserMessageProjectionEntry {
            anchor,
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
    fn persists_readonly_memory_file_idempotently() {
        let temp = tempfile::tempdir().unwrap();
        let projection = SpinetreeMemoryProjection {
            root_dir: temp.path().join(".codex/spinetree/test-session"),
        };

        projection.persist(&[entry("memory body")], &[]).unwrap();
        projection.persist(&[entry("memory body")], &[]).unwrap();

        let path = projection.root_dir.join("1.2_child_task_close_memory.md");
        assert_eq!(fs::read_to_string(&path).unwrap(), "memory body");
        assert!(fs::metadata(&path).unwrap().permissions().readonly());
        assert!(fs::symlink_metadata(path).unwrap().file_type().is_file());
        assert!(!projection.root_dir.join(".memory").exists());
    }

    #[test]
    fn rejects_existing_different_memory() {
        let temp = tempfile::tempdir().unwrap();
        let projection = SpinetreeMemoryProjection {
            root_dir: temp.path().join(".codex/spinetree/test-session"),
        };
        projection.persist(&[entry("first")], &[]).unwrap();

        let err = projection.persist(&[entry("second")], &[]).unwrap_err();
        assert!(err.to_string().contains("different content"));
    }

    #[test]
    fn appends_and_replaces_user_projection_from_complete_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let projection = SpinetreeMemoryProjection {
            root_dir: temp.path().join(".codex/spinetree/test-session"),
        };
        let path = projection.root_dir.join(USER_MESSAGES_FILENAME);

        projection.persist(&[], &[user_entry(1, "first")]).unwrap();
        projection.persist(&[], &[user_entry(1, "first")]).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "# User Messages\n\n## User Message [U1]\nfirst\n"
        );

        projection
            .persist(&[], &[user_entry(1, "first"), user_entry(2, "second")])
            .unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "# User Messages\n\n## User Message [U1]\nfirst\n\n## User Message [U2]\nsecond\n"
        );

        projection
            .persist(&[], &[user_entry(1, "replacement")])
            .unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "# User Messages\n\n## User Message [U1]\nreplacement\n"
        );

        projection.persist(&[], &[]).unwrap();
        assert_eq!(fs::read_to_string(path).unwrap(), "# User Messages\n");
    }
}
