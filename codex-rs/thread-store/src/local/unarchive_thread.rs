use codex_rollout::find_archived_thread_path_by_id_str;
use codex_rollout::read_thread_item_from_rollout;
use codex_rollout::rollout_date_parts;

use super::LocalThreadStore;
use super::helpers::SpineArtifactsMove;
use super::helpers::matching_rollout_file_name;
use super::helpers::scoped_rollout_path;
use super::helpers::stored_thread_from_rollout_item;
use super::helpers::touch_modified_time;
use crate::ArchiveThreadParams;
use crate::StoredThread;
use crate::ThreadStoreError;
use crate::ThreadStoreResult;

pub(super) async fn unarchive_thread(
    store: &LocalThreadStore,
    params: ArchiveThreadParams,
) -> ThreadStoreResult<StoredThread> {
    let thread_id = params.thread_id;
    let state_db_ctx = store.state_db().await;
    let archived_path = find_archived_thread_path_by_id_str(
        store.config.codex_home.as_path(),
        &thread_id.to_string(),
        state_db_ctx.as_deref(),
    )
    .await
    .map_err(|err| ThreadStoreError::InvalidRequest {
        message: format!("failed to locate archived thread id {thread_id}: {err}"),
    })?
    .ok_or_else(|| ThreadStoreError::InvalidRequest {
        message: format!("no archived rollout found for thread id {thread_id}"),
    })?;

    let canonical_archived_path = scoped_rollout_path(
        store
            .config
            .codex_home
            .join(codex_rollout::ARCHIVED_SESSIONS_SUBDIR),
        archived_path.as_path(),
        "archived",
    )?;
    let file_name = matching_rollout_file_name(
        canonical_archived_path.as_path(),
        thread_id,
        archived_path.as_path(),
    )?;
    let Some((year, month, day)) = rollout_date_parts(&file_name) else {
        return Err(ThreadStoreError::InvalidRequest {
            message: format!(
                "rollout path `{}` missing filename timestamp",
                archived_path.display()
            ),
        });
    };

    let dest_dir = store
        .config
        .codex_home
        .join(codex_rollout::SESSIONS_SUBDIR)
        .join(year)
        .join(month)
        .join(day);
    std::fs::create_dir_all(&dest_dir).map_err(|err| ThreadStoreError::Internal {
        message: format!("failed to unarchive thread: {err}"),
    })?;
    let restored_path = dest_dir.join(&file_name);
    let spine_move = SpineArtifactsMove::prepare(&canonical_archived_path, &restored_path)?;
    std::fs::rename(&canonical_archived_path, &restored_path).map_err(|err| {
        ThreadStoreError::Internal {
            message: format!("failed to unarchive thread: {err}"),
        }
    })?;
    if let Some(spine_move) = spine_move {
        spine_move.apply()?;
    }
    touch_modified_time(restored_path.as_path()).map_err(|err| ThreadStoreError::Internal {
        message: format!("failed to update unarchived thread timestamp: {err}"),
    })?;

    if let Some(ctx) = state_db_ctx {
        let _ = ctx
            .mark_unarchived(thread_id, restored_path.as_path())
            .await;
    }

    let item = read_thread_item_from_rollout(restored_path.clone())
        .await
        .ok_or_else(|| ThreadStoreError::Internal {
            message: format!(
                "failed to read unarchived thread {}",
                restored_path.display()
            ),
        })?;
    stored_thread_from_rollout_item(
        item,
        /*archived*/ false,
        store.config.default_model_provider_id.as_str(),
    )
    .ok_or_else(|| ThreadStoreError::Internal {
        message: format!(
            "failed to read unarchived thread id from {}",
            restored_path.display()
        ),
    })
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use codex_protocol::ThreadId;
    use codex_protocol::protocol::SessionSource;
    use pretty_assertions::assert_eq;
    use std::path::Path;
    use std::path::PathBuf;
    use tempfile::TempDir;
    use uuid::Uuid;

    use super::*;
    use crate::ThreadStore;
    use crate::local::LocalThreadStore;
    use crate::local::test_support::test_config;
    use crate::local::test_support::write_archived_session_file;

    fn write_spine_artifacts(rollout_path: &Path) -> (PathBuf, PathBuf) {
        let stem = rollout_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .expect("rollout stem");
        let parent = rollout_path.parent().expect("rollout parent");
        let locator_path = parent.join(format!("{stem}.spine.json"));
        let sidecar_path = parent.join(format!("spine-{stem}"));
        std::fs::write(
            &locator_path,
            format!(
                "{{\n  \"version\": 1,\n  \"base\": \"{}\"\n}}\n",
                sidecar_path
                    .file_name()
                    .expect("sidecar name")
                    .to_string_lossy()
            ),
        )
        .expect("write spine locator");
        std::fs::create_dir_all(&sidecar_path).expect("create spine sidecar");
        std::fs::write(sidecar_path.join("tree.jsonl"), "").expect("write spine tree");
        (locator_path, sidecar_path)
    }

    #[tokio::test]
    async fn unarchive_thread_restores_rollout_and_returns_updated_thread() {
        let home = TempDir::new().expect("temp dir");
        let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);
        let uuid = Uuid::from_u128(203);
        let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
        let archived_path = write_archived_session_file(home.path(), "2025-01-03T13-00-00", uuid)
            .expect("archived session file");

        let thread = store
            .unarchive_thread(ArchiveThreadParams { thread_id })
            .await
            .expect("unarchive thread");

        assert!(!archived_path.exists());
        let restored_path = home
            .path()
            .join("sessions/2025/01/03")
            .join(archived_path.file_name().expect("file name"));
        assert!(restored_path.exists());
        assert_eq!(thread.thread_id, thread_id);
        assert_eq!(thread.rollout_path, Some(restored_path));
        assert_eq!(thread.archived_at, None);
        assert_eq!(thread.preview, "Archived user message");
        assert_eq!(
            thread.first_user_message.as_deref(),
            Some("Archived user message")
        );
    }

    #[tokio::test]
    async fn unarchive_thread_moves_spine_locator_and_sidecar() {
        let home = TempDir::new().expect("temp dir");
        let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);
        let uuid = Uuid::from_u128(206);
        let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
        let archived_path = write_archived_session_file(home.path(), "2025-01-03T13-30-00", uuid)
            .expect("archived session file");
        let (archived_locator, archived_sidecar) = write_spine_artifacts(&archived_path);

        store
            .unarchive_thread(ArchiveThreadParams { thread_id })
            .await
            .expect("unarchive thread");

        let restored_path = home
            .path()
            .join("sessions/2025/01/03")
            .join(archived_path.file_name().expect("file name"));
        let restored_stem = restored_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .expect("restored stem");
        let restored_locator = restored_path
            .parent()
            .expect("restored parent")
            .join(format!("{restored_stem}.spine.json"));
        let restored_sidecar = restored_path
            .parent()
            .expect("restored parent")
            .join(format!("spine-{restored_stem}"));

        assert!(!archived_locator.exists());
        assert!(!archived_sidecar.exists());
        assert!(restored_locator.exists());
        assert!(restored_sidecar.join("tree.jsonl").exists());
    }

    #[tokio::test]
    async fn unarchive_thread_updates_sqlite_metadata_when_present() {
        let home = TempDir::new().expect("temp dir");
        let config = test_config(home.path());
        let uuid = Uuid::from_u128(204);
        let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
        let archived_path = write_archived_session_file(home.path(), "2025-01-03T13-00-00", uuid)
            .expect("archived session file");
        let runtime = codex_state::StateRuntime::init(
            home.path().to_path_buf(),
            config.default_model_provider_id.clone(),
        )
        .await
        .expect("state db should initialize");
        let store = LocalThreadStore::new(config.clone(), Some(runtime.clone()));
        runtime
            .mark_backfill_complete(/*last_watermark*/ None)
            .await
            .expect("backfill should be complete");
        let mut builder = codex_state::ThreadMetadataBuilder::new(
            thread_id,
            archived_path.clone(),
            Utc::now(),
            SessionSource::Cli,
        );
        builder.model_provider = Some(config.default_model_provider_id.clone());
        builder.cwd = home.path().to_path_buf();
        builder.cli_version = Some("test_version".to_string());
        let mut metadata = builder.build(config.default_model_provider_id.as_str());
        metadata.archived_at = Some(metadata.updated_at);
        runtime
            .upsert_thread(&metadata)
            .await
            .expect("state db upsert should succeed");

        store
            .unarchive_thread(ArchiveThreadParams { thread_id })
            .await
            .expect("unarchive thread");

        let restored_path = home
            .path()
            .join("sessions/2025/01/03")
            .join(archived_path.file_name().expect("file name"));
        let updated = runtime
            .get_thread(thread_id)
            .await
            .expect("state db read should succeed")
            .expect("thread metadata should exist");
        assert_eq!(updated.rollout_path, restored_path);
        assert_eq!(updated.archived_at, None);
    }
}
