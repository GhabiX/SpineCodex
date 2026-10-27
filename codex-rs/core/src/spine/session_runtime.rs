use super::coordinator::ReplayMode;
use super::coordinator::SharedSpineCoordinator;
use super::coordinator::replay_mode;
use super::coordinator::with_shared_coordinator;
use super::session_config::SpineSessionConfig;
use crate::context_manager::ContextManager;
use codex_history::ResponseItemEnvelope;
use codex_history::RolloutItem;
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

    pub(crate) fn append_response_items(
        &mut self,
        items: &[ResponseItemEnvelope],
    ) -> Result<(), String> {
        let result = with_shared_coordinator(&self.coordinator, |coordinator| {
            coordinator.observe_response_items(items)
        });
        match result {
            Some(Ok(context)) => {
                self.model_context.replace_annotated(context.items);
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

    pub(crate) fn observe_token_count(&mut self, event: TokenCountEvent, turn_id: &str) {
        with_shared_coordinator(&self.coordinator, |coordinator| {
            coordinator.observe_token_count(&event, turn_id)
        });
    }

    pub(crate) fn prepare_compact(
        &self,
        replacement_items: &[ResponseItemEnvelope],
    ) -> Result<super::coordinator::PreparedCanonicalCompact, String> {
        with_shared_coordinator(&self.coordinator, |coordinator| {
            coordinator.prepare_compact(replacement_items)
        })
        .expect("enabled Spine runtime owns a coordinator")
        .map_err(|error| error.to_string())
    }

    pub(crate) fn install_compact(
        &mut self,
        prepared: super::coordinator::PreparedCanonicalCompact,
        replacement_items: &[ResponseItemEnvelope],
    ) {
        with_shared_coordinator(&self.coordinator, |coordinator| {
            coordinator.install_compact(prepared);
        })
        .expect("enabled Spine runtime owns a coordinator");
        self.model_context
            .replace_annotated(replacement_items.to_vec());
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
        // AoT consumes the complete canonical rollout lineage, while raw_history is the host's
        // effective live context after its latest compact replacement. Replaying the full stream
        // rebuilds earlier root epochs and preserves absolute sampling coordinates; compact
        // barriers then discard obsolete context transitions before the post-compact stream is
        // projected onto raw_history.
        let mut candidate = ContextManager::new();
        candidate.replace_annotated(raw_history.annotated_items().to_vec());
        let effective = super::effective_rollout(rollout_items);
        match replay_mode(&effective) {
            Ok(ReplayMode::Native) => {
                if let Some(result) = with_shared_coordinator(&self.coordinator, |coordinator| {
                    coordinator.observe_response_items(raw_history.annotated_items())
                }) {
                    match result {
                        Ok(context) => candidate.replace_annotated(context.items),
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
                        raw_history.annotated_items(),
                        thread,
                        records,
                    ) {
                        Ok(installed) => {
                            candidate.replace_annotated(installed.context.items.clone());
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

    pub(crate) fn install_model_context(&mut self, items: Vec<ResponseItemEnvelope>) {
        self.model_context.replace_annotated(items);
    }
}
