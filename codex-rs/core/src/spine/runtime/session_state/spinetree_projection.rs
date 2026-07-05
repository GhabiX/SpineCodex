use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::path::PathBuf;

use crate::spine::io::sha1_hex;
use crate::spine::model::MemRecord;

use super::super::SpineError;

const SUMMARY_FILENAME_CHAR_BUDGET: usize = 96;

#[derive(Clone, Debug)]
pub(crate) struct SpinetreeMemoryProjectionConfig {
    root_dir: PathBuf,
}

struct ProjectionTarget {
    node_id: String,
    summary: String,
    compact_id: String,
    body_hash: String,
    target_path: PathBuf,
}

impl SpinetreeMemoryProjectionConfig {
    pub(crate) fn new(
        cwd: &Path,
        session_dir_name: String,
        _session_id: String,
        _thread_id: String,
    ) -> Self {
        Self {
            root_dir: cwd.join(".codex").join("spinetree").join(session_dir_name),
        }
    }

    pub(in crate::spine) fn persist_committed_memory(
        &self,
        mem: &MemRecord,
        summary: &str,
        target_path: PathBuf,
    ) -> Result<PathBuf, SpineError> {
        let projection = ProjectionTarget {
            node_id: mem.node.to_string(),
            summary: summary.to_string(),
            compact_id: mem.compact_id.clone(),
            body_hash: mem.body_hash.clone(),
            target_path,
        };
        validate_target(&projection)?;
        set_readonly(&projection.target_path)?;
        fs::create_dir_all(&self.root_dir)?;
        let path = self.path_for_projection(&projection);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let existing_target = resolve_symlink_target(&path, fs::read_link(&path)?);
                if existing_target != projection.target_path {
                    return Err(SpineError::InvalidStore(format!(
                        "spinetree memory projection symlink points at {}, expected {}",
                        existing_target.display(),
                        projection.target_path.display()
                    )));
                }
                Ok(path)
            }
            Ok(_) => Err(SpineError::InvalidStore(format!(
                "spinetree memory projection path already exists and is not a symlink: {}",
                path.display()
            ))),
            Err(err) if err.kind() == ErrorKind::NotFound => {
                create_file_symlink(&projection.target_path, &path)?;
                Ok(path)
            }
            Err(err) => Err(err.into()),
        }
    }

    fn path_for_projection(&self, projection: &ProjectionTarget) -> PathBuf {
        let summary = sanitize_summary_for_filename(&projection.summary);
        self.root_dir
            .join(format!("{}_{}.md", projection.node_id, summary))
    }
}

fn set_readonly(path: &Path) -> Result<(), SpineError> {
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_readonly(true);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

fn validate_target(projection: &ProjectionTarget) -> Result<(), SpineError> {
    let body = fs::read_to_string(&projection.target_path).map_err(|err| {
        SpineError::InvalidStore(format!(
            "failed to read spinetree memory projection target {} for {}: {err}",
            projection.target_path.display(),
            projection.compact_id
        ))
    })?;
    if sha1_hex(body.as_bytes()) != projection.body_hash {
        return Err(SpineError::InvalidStore(format!(
            "spinetree memory projection target hash mismatch for {}",
            projection.compact_id
        )));
    }
    Ok(())
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
fn create_file_symlink(target: &Path, link: &Path) -> Result<(), SpineError> {
    std::os::unix::fs::symlink(target, link)?;
    Ok(())
}

#[cfg(windows)]
fn create_file_symlink(target: &Path, link: &Path) -> Result<(), SpineError> {
    std::os::windows::fs::symlink_file(target, link)?;
    Ok(())
}

fn sanitize_summary_for_filename(summary: &str) -> String {
    let mut sanitized = String::new();
    let mut last_was_separator = false;
    for ch in summary.trim().chars() {
        let replacement = if ch.is_control()
            || matches!(
                ch,
                '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0'
            ) {
            Some('_')
        } else if ch.is_whitespace() {
            Some('_')
        } else {
            Some(ch)
        };
        let Some(ch) = replacement else {
            continue;
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
    use crate::spine::model::MemKind;
    use crate::spine::model::NodeId;

    fn mem(body: &str) -> MemRecord {
        MemRecord {
            compact_id: "compact-1".to_string(),
            kind: MemKind::Suffix,
            node: NodeId(vec![1, 2]),
            raw_start: 0,
            raw_end: 1,
            context_start: 0,
            context_end: 1,
            rendered_context_item_count: None,
            raw_live_hash: None,
            open_input_tokens: None,
            close_input_tokens: None,
            open_context_tokens: None,
            close_context_tokens: None,
            closed_source_suffix_tokens: None,
            closed_memory_context_tokens: None,
            open_context_source: None,
            memory_output_tokens: None,
            body_path: "memory/compact-1.md".to_string(),
            body_hash: sha1_hex(body.as_bytes()),
        }
    }

    fn persist(
        config: &SpinetreeMemoryProjectionConfig,
        summary: &str,
        target_path: PathBuf,
        body: &str,
    ) -> Result<PathBuf, SpineError> {
        config.persist_committed_memory(&mem(body), summary, target_path)
    }

    #[test]
    fn sanitizes_summary_without_losing_unicode() {
        assert_eq!(
            sanitize_summary_for_filename("  子任务 / close: memory?  "),
            "子任务_close_memory"
        );
    }

    #[test]
    fn persists_projection_as_symlink_to_readonly_sidecar_target() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("sidecar").join("memory").join("mem-1.md");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, "final memory body").unwrap();
        let config = SpinetreeMemoryProjectionConfig::new(
            temp.path(),
            "20260702_1539_session".to_string(),
            "session".to_string(),
            "thread".to_string(),
        );

        let path = persist(&config, "child memory", target.clone(), "final memory body").unwrap();

        assert_eq!(
            path,
            temp.path()
                .join(".codex")
                .join("spinetree")
                .join("20260702_1539_session")
                .join("1.2_child_memory.md")
        );
        assert!(
            fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_link(&path).unwrap(), target);
        let content = fs::read_to_string(&path).unwrap();
        assert_eq!(content, "final memory body");
        assert!(fs::metadata(path).unwrap().permissions().readonly());
    }

    #[test]
    fn accepts_existing_same_symlink_and_rejects_conflicts() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("sidecar").join("memory").join("mem-1.md");
        let other_target = temp
            .path()
            .join("sidecar")
            .join("memory")
            .join("mem-other.md");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, "body one").unwrap();
        fs::write(&other_target, "body one").unwrap();
        let config = SpinetreeMemoryProjectionConfig::new(
            temp.path(),
            "20260702_1539_session".to_string(),
            "session".to_string(),
            "thread".to_string(),
        );
        let path = persist(&config, "same", target.clone(), "body one").unwrap();
        persist(&config, "same", target.clone(), "body one").unwrap();

        let conflicting_regular = config.root_dir.join("1.2_regular.md");
        fs::write(&conflicting_regular, "different").unwrap();
        let regular_err = config
            .persist_committed_memory(&mem("body one"), "regular", target.clone())
            .unwrap_err();
        assert!(matches!(regular_err, SpineError::InvalidStore(_)));

        let conflicting_link = config.root_dir.join("1.2_other.md");
        create_file_symlink(&other_target, &conflicting_link).unwrap();
        let link_err = config
            .persist_committed_memory(&mem("body one"), "other", target.clone())
            .unwrap_err();
        assert!(matches!(link_err, SpineError::InvalidStore(_)));
        assert_eq!(fs::read_link(path).unwrap(), target);
    }
}
