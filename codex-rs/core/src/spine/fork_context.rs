//! Checkpoint native fork context edits without rewriting canonical Spine source records.
use super::*;

impl Session {
    pub(crate) async fn project_spine_fork_context(
        &self,
        prefix: &[RolloutItem],
    ) -> CodexResult<Vec<RolloutItem>> {
        use crate::spine::coordinator::CodexSpineCoordinator;
        use crate::spine::coordinator::ReplayMode;
        use crate::spine::coordinator::replay_mode;
        use crate::spine::observer::CodexSpineObserverHandler;

        let turn = self.new_default_turn().await;
        let reconstruction = self.reconstruct_history_from_rollout(&turn, prefix).await;
        let effective = crate::spine::effective_rollout(prefix);
        let ReplayMode::Canonical { thread, records } =
            replay_mode(&effective).map_err(|error| CodexErr::Fatal(error.to_string()))?
        else {
            return Err(CodexErr::Fatal(
                "Spine fork requires canonical history".to_string(),
            ));
        };
        let mut coordinator = CodexSpineCoordinator::new_with_observer(
            self.thread_id.to_string(),
            turn.config.spine.sdk().clone(),
            CodexSpineObserverHandler::default(),
        )
        .map_err(|error| CodexErr::Fatal(error.to_string()))?;
        let projection = coordinator
            .replay_canonical(&effective, &reconstruction.history, thread, records)
            .map_err(|error| CodexErr::Fatal(error.to_string()))?;
        let mut history = prefix
            .iter()
            .filter(|item| matches!(item, RolloutItem::SessionMeta(_)))
            .cloned()
            .collect::<Vec<_>>();
        if let Some(snapshot) = reconstruction.world_state_baseline {
            history.push(RolloutItem::WorldState(WorldStateItem::full(
                snapshot.into_object(),
            )));
        }
        if let Some(context) = reconstruction.reference_context_item {
            history.push(RolloutItem::TurnContext(context));
        }
        history.extend(
            projection
                .context
                .items
                .into_iter()
                .map(RolloutItem::ResponseItem),
        );
        Ok(history)
    }

    pub(crate) async fn checkpoint_spine_fork_context(
        &self,
        prefix: &[RolloutItem],
        transformed: &[RolloutItem],
    ) -> Vec<RolloutItem> {
        let turn = self.new_default_turn().await;
        let reconstruction = self
            .reconstruct_history_from_rollout(&turn, transformed)
            .await;
        let (window_number, window_ids) = self.next_auto_compact_window().await;
        let mut history = prefix.to_vec();
        history.push(RolloutItem::Compacted(CompactedItem {
            message: String::new(),
            replacement_history: Some(reconstruction.history),
            guardian_history: None,
            mcp_resource_origins: self.services.mcp_runtime.resource_origin_checkpoint(),
            window_number: Some(window_number),
            first_window_id: Some(window_ids.first_window_id.to_string()),
            previous_window_id: window_ids.previous_window_id.map(|id| id.to_string()),
            window_id: Some(window_ids.window_id.to_string()),
            compaction_response_id: None,
            latest_token_usage_record: None,
        }));
        if let Some(snapshot) = reconstruction.world_state_baseline {
            history.push(RolloutItem::WorldState(WorldStateItem::full(
                snapshot.into_object(),
            )));
        }
        if let Some(context) = reconstruction.reference_context_item {
            history.push(RolloutItem::TurnContext(context));
        }
        history
    }
}
