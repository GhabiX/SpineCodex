use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::path::PathBuf;

use crate::spine::io::sha1_hex;

use super::super::SpineError;
use super::super::prepared::SpinetreeMemoryProjection;

const SUMMARY_FILENAME_CHAR_BUDGET: usize = 96;

#[derive(Clone, Debug)]
pub(crate) struct SpinetreeMemoryProjectionConfig {
    root_dir: PathBuf,
}

impl SpinetreeMemoryProjectionConfig {
    pub(crate) fn new(
        cwd: &Path,
        session_dir_name: String,
        _session_id: String,
        _thread_id: String,
    ) -> Self {
        Self {
            root_dir: cwd.join("codex").join("spinetree").join(session_dir_name),
        }
    }

    pub(crate) fn persist(
        &self,
        projection: &SpinetreeMemoryProjection,
    ) -> Result<PathBuf, SpineError> {
        validate_target(projection)?;
        set_readonly(&projection.target_path)?;
        fs::create_dir_all(&self.root_dir)?;
        let path = self.path_for_projection(projection);
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

    fn path_for_projection(&self, projection: &SpinetreeMemoryProjection) -> PathBuf {
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

fn validate_target(projection: &SpinetreeMemoryProjection) -> Result<(), SpineError> {
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

    fn projection(summary: &str, target_path: PathBuf, body: &str) -> SpinetreeMemoryProjection {
        SpinetreeMemoryProjection {
            node_id: "1.2".to_string(),
            summary: summary.to_string(),
            compact_id: "compact-1".to_string(),
            body_hash: sha1_hex(body.as_bytes()),
            target_path,
        }
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

        let path = config
            .persist(&projection(
                "child memory",
                target.clone(),
                "final memory body",
            ))
            .unwrap();

        assert_eq!(
            path,
            temp.path()
                .join("codex")
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
        let first = projection("same", target.clone(), "body one");
        let path = config.persist(&first).unwrap();
        config.persist(&first).unwrap();

        let conflicting_regular = config.root_dir.join("1.2_regular.md");
        fs::write(&conflicting_regular, "different").unwrap();
        let regular_err = config
            .persist(&projection("regular", target.clone(), "body one"))
            .unwrap_err();
        assert!(matches!(regular_err, SpineError::InvalidStore(_)));

        let conflicting_link = config.root_dir.join("1.2_other.md");
        create_file_symlink(&other_target, &conflicting_link).unwrap();
        let link_err = config
            .persist(&projection("other", target.clone(), "body one"))
            .unwrap_err();
        assert!(matches!(link_err, SpineError::InvalidStore(_)));
        assert_eq!(fs::read_link(path).unwrap(), target);
    }
}
