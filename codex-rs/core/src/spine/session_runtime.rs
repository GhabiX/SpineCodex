use super::coordinator::ReplayMode;
use super::coordinator::SharedSpineCoordinator;
use super::coordinator::replay_mode;
use super::coordinator::with_shared_coordinator;
use super::session_config::SpineSessionConfig;
use crate::context_manager::ContextManager;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::TokenCountEvent;

pub(crate) struct SessionSpineRuntime {
    model_context: ContextManager,
    coordinator: SharedSpineCoordinator,
}

impl SessionSpineRuntime {
    pub(crate) fn new(
        configuration: &SpineSessionConfig,
        coordinator: SharedSpineCoordinator,
    ) -> Option<Self> {
        configuration.enabled().then(|| Self {
            model_context: ContextManager::new(),
            coordinator,
        })
    }

    pub(crate) fn append_response_items(&mut self, items: &[ResponseItem]) -> Result<(), String> {
        let result = with_shared_coordinator(&self.coordinator, |coordinator| {
            coordinator.observe_response_items(items)
        });
        match result {
            Some(Ok(context)) => {
                self.model_context.replace(context.items);
                Ok(())
            }
            Some(Err(error)) => {
                let reason = error.to_string();
                with_shared_coordinator(&self.coordinator, |coordinator| {
                    coordinator.latch_durability_fault(reason.clone());
                });
                Err(reason)
            }
            None => Ok(()),
        }
    }

    pub(crate) fn model_context(&self) -> ContextManager {
        self.model_context.clone()
    }

    pub(crate) fn observe_token_count(&mut self, event: TokenCountEvent) {
        with_shared_coordinator(&self.coordinator, |coordinator| {
            coordinator.observe_token_count(&event)
        });
    }

    pub(crate) fn current_input_tokens(&self) -> Option<i64> {
        with_shared_coordinator(&self.coordinator, |coordinator| {
            coordinator.current_input_tokens()
        })
        .flatten()
    }

    pub(crate) fn compact_live(
        &mut self,
        replacement_items: &[ResponseItem],
    ) -> Result<(), String> {
        let result = with_shared_coordinator(&self.coordinator, |coordinator| {
            coordinator.compact_live(replacement_items)
        });
        match result {
            Some(Ok(())) => {
                self.model_context.replace(replacement_items.to_vec());
                Ok(())
            }
            Some(Err(error)) => {
                let reason = error.to_string();
                with_shared_coordinator(&self.coordinator, |coordinator| {
                    coordinator.latch_durability_fault(reason.clone());
                });
                Err(reason)
            }
            None => Ok(()),
        }
    }

    pub(crate) fn publish_canonical_compact(&mut self) {
        with_shared_coordinator(
            &self.coordinator,
            super::coordinator::CodexSpineCoordinator::publish_canonical_compact,
        );
    }

    pub(crate) fn replay(
        &mut self,
        rollout_items: &[RolloutItem],
        raw_history: &ContextManager,
    ) -> Result<(), String> {
        let mut candidate = ContextManager::new();
        candidate.replace(raw_history.raw_items().to_vec());
        let effective = super::effective_rollout(rollout_items);
        match replay_mode(&effective) {
            Ok(ReplayMode::Native) => {
                if let Some(result) = with_shared_coordinator(&self.coordinator, |coordinator| {
                    coordinator.observe_response_items(raw_history.raw_items())
                }) {
                    match result {
                        Ok(context) => candidate.replace(context.items),
                        Err(error) => {
                            let reason = error.to_string();
                            with_shared_coordinator(&self.coordinator, |coordinator| {
                                coordinator.latch_durability_fault(reason.clone());
                            });
                            return Err(reason);
                        }
                    }
                    self.model_context = candidate;
                    return Ok(());
                }
            }
            Ok(ReplayMode::Canonical { thread, records }) => {
                let result = with_shared_coordinator(&self.coordinator, |coordinator| {
                    match coordinator.replay_canonical(
                        &effective,
                        raw_history.raw_items(),
                        thread,
                        records,
                    ) {
                        Ok(installed) => {
                            candidate.replace(installed.context.items.clone());
                            coordinator.publish_canonical_sampling(&installed);
                            Ok(())
                        }
                        Err(error) => {
                            let reason = error.to_string();
                            coordinator.latch_durability_fault(reason.clone());
                            Err(reason)
                        }
                    }
                });
                result.ok_or_else(|| {
                    "Spine coordinator is unavailable during replay".to_string()
                })??;
                self.model_context = candidate;
                return Ok(());
            }
            Err(error) => {
                let reason = format!("invalid canonical Spine rollout metadata: {error}");
                with_shared_coordinator(&self.coordinator, |coordinator| {
                    coordinator.latch_durability_fault(reason.clone());
                });
                return Err(reason);
            }
        }
        Ok(())
    }

    pub(crate) fn replace_last_turn_images(&mut self, placeholder: &str) {
        with_shared_coordinator(&self.coordinator, |coordinator| {
            coordinator.replace_last_turn_images(placeholder);
        });
    }

    pub(crate) fn install_model_context(&mut self, items: Vec<ResponseItem>) {
        self.model_context.replace(items);
    }
}
