//! Keep native resume context and canonical Spine lineage distinct at every entrypoint.
use super::*;

impl ThreadManagerState {
    pub(crate) async fn load_resumed_history(
        &self,
        metadata: &StoredThread,
        config: &Config,
    ) -> CodexResult<ResumedHistory> {
        let thread_id = metadata.thread_id;
        let (history, spine_history) = match metadata.history_mode {
            ThreadHistoryMode::Legacy => {
                let stored = match &metadata.rollout_path {
                    Some(rollout_path) => self
                        .thread_store
                        .read_thread_by_rollout_path(ReadThreadByRolloutPathParams {
                            rollout_path: rollout_path.clone(),
                            include_archived: true,
                            include_history: true,
                        })
                        .await
                        .map_err(thread_store_rollout_read_error)?,
                    None => {
                        self.read_stored_thread(ReadThreadParams {
                            thread_id,
                            include_archived: true,
                            include_history: true,
                        })
                        .await?
                    }
                };
                let history = stored.history.ok_or_else(|| {
                    CodexErr::Fatal(format!(
                        "thread {thread_id} did not include persisted history"
                    ))
                })?;
                (history.items, None)
            }
            ThreadHistoryMode::Paginated => {
                let model_context = self
                    .load_latest_model_context(LoadThreadHistoryParams {
                        thread_id,
                        include_archived: true,
                    })
                    .await?;
                let spine_history = if config.features.enabled(codex_features::Feature::SpineJit) {
                    let complete = self
                        .thread_store
                        .load_complete_history(LoadThreadHistoryParams {
                            thread_id,
                            include_archived: true,
                        })
                        .await
                        .map_err(thread_store_rollout_read_error)?;
                    Some(Arc::new(complete.items))
                } else {
                    None
                };
                (model_context.items, spine_history)
            }
        };
        Ok(ResumedHistory {
            conversation_id: thread_id,
            history: Arc::new(history),
            spine_history,
            rollout_path: metadata.rollout_path.clone(),
        })
    }
}
