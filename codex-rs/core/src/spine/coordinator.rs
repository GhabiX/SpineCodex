use super::context_handler::response_item_to_char_and_source;
use super::context_plan::CodexContextPlanError;
use super::context_plan::PreparedCodexContextPlan;
use super::context_plan::prepare_codex_context_plan;
use super::memory_projection::SpinetreeUserMessageProjectionEntry;
use super::observer::CodexSpineObserverHandler;
use crate::session::session::Session;
use codex_history::ResponseItemEnvelope;
use codex_history::RolloutItem;
use codex_history::SpineTransitionItem;
use codex_protocol::models::ResponseItem;
use spine_core::host::CanonicalReplay;
use spine_core::host::ContextEpoch;
use spine_core::host::ContextWindowSample;
use spine_core::host::PreparedSamplingCommit;
use spine_core::host::RawBoundary;
use spine_core::host::ReplayInput;
use spine_core::host::SamplingArchiveRecord;
use spine_core::host::SamplingFinish;
use spine_core::host::SamplingHandle;
use spine_core::host::SamplingRuntime;
use spine_core::host::SamplingTerminal;
use spine_core::host::SpineCompactBarrierV1;
use spine_core::host::SpineConfig;
use spine_core::host::SpineOperationFact;
use spine_core::host::SpineProjection;
use spine_core::host::ThreadNamespace;
use spine_core::host::TokenUsageSample;
use std::collections::BTreeMap;
use std::sync::Arc;

mod archive;
mod compact;
pub(crate) use compact::PreparedCanonicalCompact;
mod replay;
mod session;

pub(crate) use archive::CoordinatorError;
pub(crate) use archive::ReplayMode;
use archive::ReplaySeedItem;
pub(crate) use archive::decode_spine_rollout_item;
use archive::encode_spine_sampling_started;
use archive::encode_spine_transition;
pub(crate) use archive::replay_mode;
pub(crate) use session::SpineSessionAdapter;

pub(crate) type SpineSamplingAttempt = SamplingHandle;
pub(crate) type SharedSpineCoordinator = Arc<std::sync::Mutex<Option<CodexSpineCoordinator>>>;

pub(crate) fn with_shared_coordinator<R>(
    coordinator: &SharedSpineCoordinator,
    f: impl FnOnce(&mut CodexSpineCoordinator) -> R,
) -> Option<R> {
    coordinator
        .lock()
        .unwrap_or_else(|_| panic!("Spine coordinator mutex must not be poisoned"))
        .as_mut()
        .map(f)
}

pub(crate) struct CanonicalSamplingCommit {
    transition: SpineTransitionItem,
    prepared: PreparedSamplingCommit,
    context: PreparedCodexContextPlan,
}

impl CanonicalSamplingCommit {
    pub(crate) fn rollout_item(&self) -> RolloutItem {
        RolloutItem::SpineTransition(self.transition.clone())
    }
}

#[derive(Debug)]
pub(crate) struct InstalledCanonicalCommit {
    pub(crate) context: PreparedCodexContextPlan,
    pub(crate) projection: SpineProjection,
    pub(crate) settled_spawn_call_ids: Vec<String>,
}

pub(crate) struct CodexSpineCoordinator {
    pub(crate) runtime: SamplingRuntime,
    runtime_config: SpineConfig,
    next_boundary: u64,
    source_items: BTreeMap<spine_core::host::SourceCellId, ResponseItemEnvelope>,
    replay_seed: Option<Vec<ReplaySeedItem>>,
    node_prompt: String,
    pub(crate) durability_fault: Option<String>,
    observer: CodexSpineObserverHandler,
    usage_samples: Vec<TokenUsageSample>,
    context_window_samples: Vec<ContextWindowSample>,
    user_messages: Vec<SpinetreeUserMessageProjectionEntry>,
}

impl CodexSpineCoordinator {
    pub(crate) fn jit_enabled(&self) -> bool {
        self.runtime_config
            .is_enabled(spine_core::host::Feature::Jit)
    }

    pub(crate) fn new_with_observer(
        thread: impl Into<String>,
        config: SpineConfig,
        observer: CodexSpineObserverHandler,
    ) -> Result<Self, CoordinatorError> {
        let thread = ThreadNamespace::parse(thread.into())
            .map_err(|error| CoordinatorError::Identity(error.to_string()))?;
        let node_prompt = config.node_prompt().unwrap_or_default().to_string();
        let runtime = SamplingRuntime::new(thread, ContextEpoch::ZERO, config.clone())?;
        Ok(Self {
            runtime,
            runtime_config: config,
            next_boundary: 0,
            source_items: BTreeMap::new(),
            replay_seed: Some(Vec::new()),
            node_prompt,
            durability_fault: None,
            observer,
            usage_samples: Vec::new(),
            context_window_samples: Vec::new(),
            user_messages: Vec::new(),
        })
    }

    pub(crate) fn observe_response_items(
        &mut self,
        items: &[ResponseItemEnvelope],
    ) -> Result<PreparedCodexContextPlan, CoordinatorError> {
        self.require_healthy()?;
        let mut characters = Vec::with_capacity(items.len());
        let mut projected_items = Vec::with_capacity(items.len());
        for item in items {
            let boundary = RawBoundary(self.next_boundary);
            self.next_boundary = self.next_boundary.saturating_add(1);
            let (character, projected) = response_item_to_char_and_source(item, boundary);
            if let Some(seed) = &mut self.replay_seed {
                seed.push(ReplaySeedItem::Source {
                    boundary: boundary.0,
                    item: RolloutItem::ResponseItem(item.clone()),
                });
            }
            characters.push(character);
            projected_items.push(projected);
        }
        let source_ids = self.runtime.observe_source(characters)?;
        self.source_items
            .extend(source_ids.into_iter().zip(projected_items));
        self.prepare_live_context()
    }

    fn prepare_live_context(&self) -> Result<PreparedCodexContextPlan, CoordinatorError> {
        let plan = self.runtime.preview_context_plan()?;
        let snapshot = self.runtime.source_snapshot();
        let node_context_costs = self
            .runtime
            .node_context_costs(&self.context_window_samples);
        Ok(prepare_codex_context_plan(
            &plan,
            &snapshot,
            &self.source_items,
            &node_context_costs,
            &self.node_prompt,
        )?)
    }

    pub(crate) fn begin_sampling(&mut self) -> Result<SpineSamplingAttempt, CoordinatorError> {
        self.require_healthy()?;
        Ok(self.runtime.begin_sampling()?)
    }

    pub(crate) fn has_pending_durable_sampling(&self) -> bool {
        self.runtime.has_pending_durable_sampling()
    }

    pub(crate) fn sampling_started_rollout_item(
        &mut self,
        attempt: &SpineSamplingAttempt,
        prompt: &[ResponseItem],
    ) -> Result<RolloutItem, CoordinatorError> {
        Ok(RolloutItem::SpineSamplingStarted(
            self.sampling_started_item(attempt, prompt)?,
        ))
    }

    fn sampling_started_item(
        &mut self,
        attempt: &SpineSamplingAttempt,
        prompt: &[ResponseItem],
    ) -> Result<codex_history::SpineSamplingStartedItem, CoordinatorError> {
        let _ = prompt;
        let record = self.runtime.sampling_started_record(attempt)?;
        let mut started = encode_spine_sampling_started(&record)?;
        started.sdk_config = Some(
            self.runtime_config
                .snapshot_toml()
                .map_err(|error| CoordinatorError::Codec(error.to_string()))?,
        );
        if let Some(seed) = self.replay_seed.take() {
            started.replay_seed = Some(
                serde_json::to_value(seed)
                    .map_err(|error| CoordinatorError::Codec(error.to_string()))?,
            );
        }
        Ok(started)
    }

    pub(crate) fn abort_sampling(
        &mut self,
        attempt: &SpineSamplingAttempt,
    ) -> Result<(), CoordinatorError> {
        Ok(self.runtime.abort_sampling(attempt)?)
    }

    #[cfg(test)]
    pub(crate) fn finish_canonical_sampling(
        &mut self,
        attempt: SpineSamplingAttempt,
        terminal: SamplingTerminal,
    ) -> Result<Option<CanonicalSamplingCommit>, CoordinatorError> {
        self.finish_canonical_sampling_with_input_tokens(attempt, terminal, None)
    }

    pub(crate) fn finish_canonical_sampling_with_input_tokens(
        &mut self,
        attempt: SpineSamplingAttempt,
        terminal: SamplingTerminal,
        input_tokens: Option<i64>,
    ) -> Result<Option<CanonicalSamplingCommit>, CoordinatorError> {
        self.require_healthy()?;
        let durable_input_tokens = input_tokens.and_then(|tokens| u64::try_from(tokens).ok());
        let SamplingFinish::Prepared(prepared) = self.runtime.finish_sampling_with_input_tokens(
            attempt,
            terminal,
            durable_input_tokens,
        )?
        else {
            return Ok(None);
        };
        let transition = match encode_spine_transition(&SamplingArchiveRecord::SamplingCommit(
            prepared.durable_record().clone(),
        )) {
            Ok(transition) => transition,
            Err(error) => {
                self.runtime.discard_unpersisted_prepared(&prepared)?;
                return Err(error);
            }
        };
        let node_context_costs = prepared.node_context_costs(&self.context_window_samples);
        let context = match prepare_codex_context_plan(
            prepared.context_plan(),
            &self.runtime.source_snapshot(),
            &self.source_items,
            &node_context_costs,
            &self.node_prompt,
        ) {
            Ok(context) => context,
            Err(error) => {
                self.runtime.discard_unpersisted_prepared(&prepared)?;
                return Err(error.into());
            }
        };
        Ok(Some(CanonicalSamplingCommit {
            transition,
            prepared,
            context,
        }))
    }

    #[cfg(test)]
    pub(crate) fn current_input_tokens(&self) -> Option<i64> {
        self.runtime
            .current_input_tokens()
            .and_then(|tokens| i64::try_from(tokens).ok())
    }

    #[cfg(test)]
    pub(crate) fn user_message_projection(&self) -> &[SpinetreeUserMessageProjectionEntry] {
        &self.user_messages
    }

    #[cfg(test)]
    pub(crate) fn prepare_canonical_sampling(
        &mut self,
        attempt: SpineSamplingAttempt,
    ) -> Result<CanonicalSamplingCommit, CoordinatorError> {
        self.finish_canonical_sampling(attempt, SamplingTerminal::Completed)?
            .ok_or_else(|| {
                CoordinatorError::Replay(
                    "completed sampling unexpectedly produced no commit".to_string(),
                )
            })
    }

    pub(crate) fn install_canonical_sampling(
        &mut self,
        commit: CanonicalSamplingCommit,
    ) -> Result<InstalledCanonicalCommit, CoordinatorError> {
        let CanonicalSamplingCommit {
            prepared, context, ..
        } = commit;
        let settled_spawn_call_ids = prepared
            .durable_record()
            .executions
            .iter()
            .filter(|&execution| matches!(execution.operation, SpineOperationFact::Spawn { .. }))
            .map(|execution| match &execution.origin {
                spine_core::host::ExecutionOrigin::Direct { execution_ref } => {
                    execution_ref.clone()
                }
            })
            .collect();
        let output = self.runtime.install_prepared(prepared)?;
        Ok(InstalledCanonicalCommit {
            context,
            projection: output.projection,
            settled_spawn_call_ids,
        })
    }

    pub(crate) fn publish_canonical_sampling(&mut self, commit: &InstalledCanonicalCommit) {
        self.user_messages = commit.context.user_messages.clone();
        let event_id = commit
            .context
            .items
            .iter()
            .rev()
            .find_map(|item| item.item.turn_id());
        self.observer.publish_committed(
            &commit.projection,
            &self.usage_samples,
            event_id,
            self.user_messages.clone(),
            &commit.settled_spawn_call_ids,
        );
    }

    pub(crate) fn publish_canonical_compact(&mut self) {
        self.observer.publish_committed(
            self.runtime.projection(),
            &self.usage_samples,
            None,
            self.user_messages.clone(),
            &[],
        );
    }

    pub(crate) fn observe_token_count(
        &mut self,
        event: &codex_protocol::protocol::TokenCountEvent,
        turn_id: &str,
    ) {
        let Some(info) = event.info.as_ref() else {
            return;
        };
        if let Some(seed) = &mut self.replay_seed {
            seed.push(ReplaySeedItem::Usage {
                boundary: self.next_boundary,
                input_tokens: info.last_token_usage.input_tokens,
                model_context_window: info.model_context_window,
            });
        }
        if let Some(model_context_window) = info.model_context_window {
            self.record_context_window(model_context_window);
        }
        self.usage_samples.push(TokenUsageSample {
            boundary: RawBoundary(self.next_boundary),
            input_tokens: info.last_token_usage.input_tokens,
        });
        self.observer
            .publish_usage(self.runtime.projection(), &self.usage_samples, turn_id);
    }

    pub(crate) fn record_context_window(&mut self, model_context_window: i64) {
        let sample = ContextWindowSample {
            boundary: RawBoundary(self.next_boundary),
            model_context_window,
        };
        if self.context_window_samples.last() != Some(&sample) {
            self.context_window_samples.push(sample);
        }
    }

    pub(crate) fn register_execution(&mut self, key: &str) -> Result<(), CoordinatorError> {
        self.require_healthy()?;
        Ok(self.runtime.register_execution(key)?)
    }

    pub(crate) fn stage_execution(
        &mut self,
        key: &str,
        origin: spine_core::host::ExecutionOrigin,
        operation: SpineOperationFact,
    ) -> Result<(), CoordinatorError> {
        self.require_healthy()?;
        Ok(self.runtime.stage_execution(key, origin, operation)?)
    }

    pub(crate) fn validate_control(&self, tool: spine_core::host::SpineTool) -> Result<(), String> {
        self.require_healthy().map_err(|error| error.to_string())?;
        if matches!(
            tool,
            spine_core::host::SpineTool::Close | spine_core::host::SpineTool::Next
        ) {
            let projection = self.runtime.projection();
            let cursor = projection
                .nodes
                .iter()
                .find(|node| node.id == projection.cursor)
                .ok_or_else(|| "Spine cursor is missing from the derived tree".to_string())?;
            if cursor.kind == spine_core::host::NodeKind::RootEpoch {
                return Err("no open Spine node is available to close".to_string());
            }
        }
        Ok(())
    }

    pub(crate) fn finish_execution(
        &mut self,
        key: &str,
        succeeded: bool,
    ) -> Result<(), CoordinatorError> {
        self.require_healthy()?;
        Ok(self.runtime.finish_execution(key, succeeded)?)
    }

    pub(crate) fn latch_durability_fault(&mut self, reason: impl Into<String>) {
        if self.durability_fault.is_none() {
            self.durability_fault = Some(reason.into());
        }
    }

    fn require_healthy(&self) -> Result<(), CoordinatorError> {
        if let Some(reason) = &self.durability_fault {
            return Err(CoordinatorError::DurabilityFaulted(reason.clone()));
        }
        Ok(())
    }
}
