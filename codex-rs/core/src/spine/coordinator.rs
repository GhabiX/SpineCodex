use super::context_handler::SamplingReplies;
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
use spine_core::host::NodeId;
use spine_core::host::PreparedSamplingCommit;
use spine_core::host::RawBoundary;
use spine_core::host::ReplayInput;
use spine_core::host::SamplingArchiveRecord;
use spine_core::host::SamplingFinish;
use spine_core::host::SamplingHandle;
use spine_core::host::SamplingRuntime;
use spine_core::host::SamplingTerminal;
use spine_core::host::SpawnArrival;
use spine_core::host::SpawnState;
use spine_core::host::SpineChar;
use spine_core::host::SpineCompactBarrierV1;
use spine_core::host::SpineConfig;
use spine_core::host::SpineOperationFact;
use spine_core::host::SpineProjection;
use spine_core::host::ThreadNamespace;
use spine_core::host::TokenUsageSample;
use std::collections::BTreeMap;
use std::sync::Arc;

mod archive;
mod births;
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
use births::SpawnBirths;
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
    /// The sampling in progress holds its assistant messages until it ends.
    replies: Option<SamplingReplies<ResponseItemEnvelope>>,
    /// Counts every change to the observed source, so a cached live context
    /// can tell that it is stale.
    source_revision: u64,
    /// Producers whose arrivals have entered the source, with the child
    /// thread each one names.
    entered_arrivals: BTreeMap<NodeId, Option<String>>,
    births: SpawnBirths,
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
            replies: None,
            source_revision: 0,
            entered_arrivals: BTreeMap::new(),
            births: SpawnBirths::default(),
        })
    }

    pub(crate) fn observe_response_items(
        &mut self,
        items: &[ResponseItemEnvelope],
    ) -> Result<PreparedCodexContextPlan, CoordinatorError> {
        self.require_healthy()?;
        let mut settled = Vec::with_capacity(items.len());
        for item in items {
            let boundary = RawBoundary(self.next_boundary);
            self.next_boundary = self.next_boundary.saturating_add(1);
            let (character, projected) = response_item_to_char_and_source(item, boundary)
                .map_err(CoordinatorError::Codec)?;
            if let Some(seed) = &mut self.replay_seed {
                seed.push(ReplaySeedItem::Source {
                    boundary: boundary.0,
                    item: RolloutItem::ResponseItem(item.clone()),
                });
            }
            match &mut self.replies {
                Some(replies) => settled.extend(replies.observe(character, projected)),
                None => settled.push((character, projected)),
            }
        }
        self.observe_settled(settled)?;
        self.prepare_live_context()
    }

    fn observe_settled(
        &mut self,
        settled: Vec<(SpineChar, ResponseItemEnvelope)>,
    ) -> Result<(), CoordinatorError> {
        if settled.is_empty() {
            return Ok(());
        }
        let (characters, projected_items): (Vec<_>, Vec<_>) = settled.into_iter().unzip();
        self.entered_arrivals
            .extend(characters.iter().filter_map(|character| match character {
                SpineChar::Arrival { arrival, .. } => {
                    Some((arrival.producer.clone(), arrival.execution_ref.clone()))
                }
                _ => None,
            }));
        let source_ids = self.runtime.observe_source(characters)?;
        self.source_items
            .extend(source_ids.into_iter().zip(projected_items));
        self.source_revision = self.source_revision.wrapping_add(1);
        Ok(())
    }

    /// Ends the sampling's hold on its assistant messages.
    fn settle_replies(&mut self, completed: bool) -> Result<(), CoordinatorError> {
        match self.replies.take() {
            Some(replies) => self.observe_settled(replies.finish(completed)),
            None => Ok(()),
        }
    }

    /// The child threads whose results have arrived; their subtrees are settled.
    pub(crate) fn arrived_spawn_threads(&self) -> Vec<codex_protocol::ThreadId> {
        self.entered_arrivals
            .values()
            .flatten()
            .filter_map(|thread_id| codex_protocol::ThreadId::from_string(thread_id).ok())
            .collect()
    }

    pub(crate) fn source_revision(&self) -> u64 {
        self.source_revision
    }

    pub(crate) fn prepare_live_context(
        &self,
    ) -> Result<PreparedCodexContextPlan, CoordinatorError> {
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
        let attempt = self.runtime.begin_sampling()?;
        self.replies = Some(SamplingReplies::default());
        Ok(attempt)
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
        self.settle_replies(false)?;
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
        let completed = terminal == SamplingTerminal::Completed;
        self.settle_replies(completed)?;
        let durable_input_tokens = input_tokens.and_then(|tokens| u64::try_from(tokens).ok());
        let SamplingFinish::Prepared(prepared) = self.runtime.finish_sampling_with_input_tokens(
            attempt,
            terminal,
            durable_input_tokens,
        )?
        else {
            return Ok(None);
        };
        let transition = match encode_spine_transition(
            &SamplingArchiveRecord::SamplingCommit(prepared.durable_record().clone()),
            completed,
        ) {
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
        let spawns = prepared
            .durable_record()
            .executions
            .iter()
            .filter_map(
                |execution| match (&execution.operation, &execution.origin) {
                    (
                        SpineOperationFact::Spawn { tasks },
                        spine_core::host::ExecutionOrigin::Direct { execution_ref },
                    ) => Some((execution_ref.clone(), tasks.len())),
                    _ => None,
                },
            )
            .collect::<Vec<_>>();
        let output = self.runtime.install_prepared(prepared)?;
        self.bear_spawns(&output.projection, spawns)?;
        Ok(InstalledCanonicalCommit {
            context,
            projection: output.projection,
            settled_spawn_call_ids,
        })
    }

    /// A committed Spawn appends its births, in call order, to the cursor's
    /// children.
    fn bear_spawns(
        &mut self,
        projection: &SpineProjection,
        spawns: Vec<(String, usize)>,
    ) -> Result<(), CoordinatorError> {
        let born: usize = spawns.iter().map(|(_, count)| count).sum();
        if born == 0 {
            return Ok(());
        }
        let cursor = projection
            .nodes
            .iter()
            .find(|node| node.id == projection.cursor)
            .ok_or_else(|| {
                CoordinatorError::Replay(
                    "Spine cursor is missing from the derived tree".to_string(),
                )
            })?;
        let mut births = cursor.children[cursor.children.len() - born..]
            .iter()
            .cloned();
        for (call_id, count) in spawns {
            self.births
                .bear(call_id, births.by_ref().take(count).collect());
        }
        Ok(())
    }

    /// Records a spine.spawn call's child results for delivery once their
    /// births have committed.
    pub(crate) fn finish_spawn(
        &mut self,
        call_id: String,
        results: Vec<crate::spine::spawn::SpawnResult>,
    ) {
        self.births.finish(call_id, results);
    }

    pub(crate) fn take_spawn_arrivals(&mut self) -> Vec<SpawnArrival> {
        self.births.take_arrivals()
    }

    /// Births within `scope` whose arrivals have not entered the source, as
    /// `id (summary)`.
    fn unarrived_births(&self, scope: &NodeId) -> Vec<String> {
        self.runtime
            .projection()
            .nodes
            .iter()
            .filter(|node| {
                node.spawn == Some(SpawnState::Pending)
                    && node.id.is_within(scope)
                    && !self.entered_arrivals.contains_key(&node.id)
            })
            .map(|node| {
                format!(
                    "{} ({})",
                    node.id,
                    node.summary.as_deref().unwrap_or_default()
                )
            })
            .collect()
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
            &self.arrived_spawn_threads(),
        );
    }

    pub(crate) fn publish_canonical_compact(&mut self) {
        self.observer.publish_committed(
            self.runtime.projection(),
            &self.usage_samples,
            None,
            self.user_messages.clone(),
            &[],
            &self.arrived_spawn_threads(),
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
        self.observer.publish_usage(
            self.runtime.projection(),
            &self.usage_samples,
            turn_id,
            &self.arrived_spawn_threads(),
        );
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
            let pending = self.unarrived_births(&projection.cursor);
            if !pending.is_empty() {
                return Err(format!(
                    "Spawned children {} have not arrived. Each result arrives as its own \
                     message; close this node after they have arrived.",
                    pending.join(", ")
                ));
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
