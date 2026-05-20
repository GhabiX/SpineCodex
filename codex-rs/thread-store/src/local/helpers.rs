use std::ffi::OsStr;
use std::fs::FileTimes;
use std::fs::OpenOptions;
use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;

use chrono::DateTime;
use chrono::Utc;
use codex_git_utils::GitSha;
use codex_protocol::ThreadId;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::GitInfo;
use codex_protocol::protocol::SandboxPolicy;
use codex_protocol::protocol::SessionSource;
use codex_rollout::ARCHIVED_SESSIONS_SUBDIR;
use codex_rollout::ThreadItem;
use codex_state::ThreadMetadata;
use serde::Deserialize;

use crate::StoredThread;
use crate::ThreadStoreError;
use crate::ThreadStoreResult;

pub(super) fn scoped_rollout_path(
    root: PathBuf,
    rollout_path: &Path,
    root_name: &str,
) -> ThreadStoreResult<PathBuf> {
    let canonical_root =
        std::fs::canonicalize(&root).map_err(|err| ThreadStoreError::Internal {
            message: format!(
                "failed to resolve {root_name} directory `{}`: {err}",
                root.display()
            ),
        })?;
    let canonical_rollout_path =
        std::fs::canonicalize(rollout_path).map_err(|_| ThreadStoreError::InvalidRequest {
            message: format!(
                "rollout path `{}` must be in {root_name} directory",
                rollout_path.display()
            ),
        })?;
    if canonical_rollout_path.starts_with(&canonical_root) {
        Ok(canonical_rollout_path)
    } else {
        Err(ThreadStoreError::InvalidRequest {
            message: format!(
                "rollout path `{}` must be in {root_name} directory",
                rollout_path.display()
            ),
        })
    }
}

pub(super) fn rollout_path_is_archived(codex_home: &Path, path: &Path) -> bool {
    path.starts_with(codex_home.join(ARCHIVED_SESSIONS_SUBDIR))
        || path
            .components()
            .any(|component| component.as_os_str() == OsStr::new(ARCHIVED_SESSIONS_SUBDIR))
}

pub(super) fn matching_rollout_file_name(
    rollout_path: &Path,
    thread_id: ThreadId,
    display_path: &Path,
) -> ThreadStoreResult<std::ffi::OsString> {
    let Some(file_name) = rollout_path.file_name().map(OsStr::to_owned) else {
        return Err(ThreadStoreError::InvalidRequest {
            message: format!(
                "rollout path `{}` missing file name",
                display_path.display()
            ),
        });
    };
    let required_suffix = format!("{thread_id}.jsonl");
    if file_name
        .to_string_lossy()
        .ends_with(required_suffix.as_str())
    {
        Ok(file_name)
    } else {
        Err(ThreadStoreError::InvalidRequest {
            message: format!(
                "rollout path `{}` does not match thread id {thread_id}",
                display_path.display()
            ),
        })
    }
}

pub(super) struct SpineArtifactsMove {
    source_locator: PathBuf,
    destination_locator: PathBuf,
    source_sidecar: PathBuf,
    destination_sidecar: PathBuf,
}

const SPINE_RELOCATION_INVALID_SIDECAR: &str = "SpineRelocationInvalidSidecar";

impl SpineArtifactsMove {
    pub(super) fn prepare(
        source_rollout_path: &Path,
        destination_rollout_path: &Path,
    ) -> ThreadStoreResult<Option<Self>> {
        let source_locator = spine_locator_path_for_rollout(source_rollout_path)?;
        if !source_locator.exists() {
            return Ok(None);
        }
        let destination_locator = spine_locator_path_for_rollout(destination_rollout_path)?;
        if destination_locator.exists() {
            return Err(ThreadStoreError::Internal {
                message: format!(
                    "spine base locator target already exists: {}",
                    destination_locator.display()
                ),
            });
        }

        let base = read_spine_base_locator(&source_locator)?;
        let source_sidecar = source_locator
            .parent()
            .expect("locator has parent")
            .join(&base);
        validate_spine_sidecar(&source_sidecar)?;
        let destination_sidecar = destination_locator
            .parent()
            .expect("locator has parent")
            .join(&base);
        if destination_sidecar.exists() {
            return Err(ThreadStoreError::Internal {
                message: format!(
                    "spine sidecar target already exists: {}",
                    destination_sidecar.display()
                ),
            });
        }

        Ok(Some(Self {
            source_locator,
            destination_locator,
            source_sidecar,
            destination_sidecar,
        }))
    }

    pub(super) fn apply(self) -> ThreadStoreResult<()> {
        if let Some(parent) = self.destination_locator.parent() {
            std::fs::create_dir_all(parent).map_err(|err| ThreadStoreError::Internal {
                message: format!("failed to create spine locator target directory: {err}"),
            })?;
        }
        if let Some(parent) = self.destination_sidecar.parent() {
            std::fs::create_dir_all(parent).map_err(|err| ThreadStoreError::Internal {
                message: format!("failed to create spine sidecar target directory: {err}"),
            })?;
        }
        std::fs::rename(&self.source_sidecar, &self.destination_sidecar).map_err(|err| {
            ThreadStoreError::Internal {
                message: format!("failed to move spine sidecar: {err}"),
            }
        })?;
        std::fs::rename(&self.source_locator, &self.destination_locator).map_err(|err| {
            ThreadStoreError::Internal {
                message: format!("failed to move spine base locator: {err}"),
            }
        })
    }
}

fn spine_locator_path_for_rollout(rollout_path: &Path) -> ThreadStoreResult<PathBuf> {
    let parent = rollout_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| ThreadStoreError::InvalidRequest {
            message: format!(
                "rollout path `{}` missing parent directory",
                rollout_path.display()
            ),
        })?;
    let stem = rollout_path
        .file_stem()
        .and_then(OsStr::to_str)
        .filter(|stem| !stem.is_empty())
        .ok_or_else(|| ThreadStoreError::InvalidRequest {
            message: format!(
                "rollout path `{}` missing valid UTF-8 file stem",
                rollout_path.display()
            ),
        })?;
    Ok(parent.join(format!("{stem}.spine.json")))
}

#[derive(Deserialize)]
struct SpineBaseLocator {
    version: u32,
    base: PathBuf,
}

fn read_spine_base_locator(path: &Path) -> ThreadStoreResult<PathBuf> {
    let contents = std::fs::read_to_string(path).map_err(|err| ThreadStoreError::Internal {
        message: format!(
            "failed to read spine base locator {}: {err}",
            path.display()
        ),
    })?;
    let locator: SpineBaseLocator =
        serde_json::from_str(&contents).map_err(|err| ThreadStoreError::Internal {
            message: format!(
                "failed to parse spine base locator {}: {err}",
                path.display()
            ),
        })?;
    if locator.version != 1 {
        return Err(ThreadStoreError::Internal {
            message: format!(
                "unsupported spine base locator version {} at {}",
                locator.version,
                path.display()
            ),
        });
    }
    validate_spine_base(&locator.base, path)?;
    Ok(locator.base)
}

fn validate_spine_base(base: &Path, locator_path: &Path) -> ThreadStoreResult<()> {
    if base.as_os_str().is_empty() || base.is_absolute() {
        return Err(ThreadStoreError::Internal {
            message: format!(
                "spine base locator {} must contain a non-empty relative base",
                locator_path.display()
            ),
        });
    }
    if base.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir
                | std::path::Component::RootDir
                | std::path::Component::Prefix(_)
        )
    }) {
        return Err(ThreadStoreError::Internal {
            message: format!(
                "spine base locator {} must stay within the rollout directory",
                locator_path.display()
            ),
        });
    }
    Ok(())
}

fn validate_spine_sidecar(sidecar_path: &Path) -> ThreadStoreResult<()> {
    if !sidecar_path.is_dir() {
        return Err(ThreadStoreError::Internal {
            message: format!(
                "{SPINE_RELOCATION_INVALID_SIDECAR}: spine sidecar directory is missing: {}",
                sidecar_path.display()
            ),
        });
    }
    let tree_path = sidecar_path.join("tree.jsonl");
    if !tree_path.is_file() {
        return Err(ThreadStoreError::Internal {
            message: format!(
                "{SPINE_RELOCATION_INVALID_SIDECAR}: spine sidecar is missing tree.jsonl: {}",
                tree_path.display()
            ),
        });
    }
    Ok(())
}

pub(super) fn touch_modified_time(path: &Path) -> std::io::Result<()> {
    let times = FileTimes::new().set_modified(SystemTime::now());
    OpenOptions::new().append(true).open(path)?.set_times(times)
}

pub(super) fn stored_thread_from_rollout_item(
    item: ThreadItem,
    archived: bool,
    default_provider: &str,
) -> Option<StoredThread> {
    let thread_id = item
        .thread_id
        .or_else(|| thread_id_from_rollout_path(item.path.as_path()))?;
    let created_at = parse_rfc3339(item.created_at.as_deref()).unwrap_or_else(Utc::now);
    let updated_at = parse_rfc3339(item.updated_at.as_deref()).unwrap_or(created_at);
    let archived_at = archived.then_some(updated_at);
    let git_info = git_info_from_parts(
        item.git_sha.clone(),
        item.git_branch.clone(),
        item.git_origin_url.clone(),
    );
    let source = item.source.unwrap_or(SessionSource::Unknown);
    let preview = item.first_user_message.clone().unwrap_or_default();

    Some(StoredThread {
        thread_id,
        rollout_path: Some(item.path),
        forked_from_id: None,
        preview,
        name: None,
        model_provider: item
            .model_provider
            .filter(|provider| !provider.is_empty())
            .unwrap_or_else(|| default_provider.to_string()),
        model: None,
        reasoning_effort: None,
        created_at,
        updated_at,
        archived_at,
        cwd: item.cwd.unwrap_or_default(),
        cli_version: item.cli_version.unwrap_or_default(),
        source,
        thread_source: None,
        agent_nickname: item.agent_nickname,
        agent_role: item.agent_role,
        agent_path: None,
        git_info,
        approval_mode: AskForApproval::OnRequest,
        sandbox_policy: SandboxPolicy::new_read_only_policy(),
        token_usage: None,
        first_user_message: item.first_user_message,
        history: None,
    })
}

pub(super) fn distinct_thread_metadata_title(metadata: &ThreadMetadata) -> Option<String> {
    let title = metadata.title.trim();
    if title.is_empty() || metadata.first_user_message.as_deref().map(str::trim) == Some(title) {
        None
    } else {
        Some(title.to_string())
    }
}

pub(super) fn set_thread_name_from_title(thread: &mut StoredThread, title: String) {
    if title.trim().is_empty() || thread.preview.trim() == title.trim() {
        return;
    }
    thread.name = Some(title);
}

fn parse_rfc3339(value: Option<&str>) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value?)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

pub(super) fn git_info_from_parts(
    sha: Option<String>,
    branch: Option<String>,
    origin_url: Option<String>,
) -> Option<GitInfo> {
    if sha.is_none() && branch.is_none() && origin_url.is_none() {
        return None;
    }
    Some(GitInfo {
        commit_hash: sha.as_deref().map(GitSha::new),
        branch,
        repository_url: origin_url,
    })
}

fn thread_id_from_rollout_path(path: &Path) -> Option<ThreadId> {
    let file_name = path.file_name()?.to_str()?;
    let stem = file_name.strip_suffix(".jsonl")?;
    if stem.len() < 37 {
        return None;
    }
    let uuid_start = stem.len().saturating_sub(36);
    if !stem[..uuid_start].ends_with('-') {
        return None;
    }
    ThreadId::from_string(&stem[uuid_start..]).ok()
}
