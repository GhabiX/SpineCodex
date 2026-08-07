use super::context_handler::response_item_to_char_and_source;
use super::context_plan::CodexContextPlanError;
use super::context_plan::PreparedCodexContextPlan;
use super::context_plan::prepare_codex_context_plan;
use super::memory_projection::SpinetreeUserMessageProjectionEntry;
use crate::session::session::Session;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::SpineTransitionItem;
use spine_core::CanonicalReplay;
use spine_core::ContextEpoch;
use spine_core::ContextWindowSample;
use spine_core::PreparedSamplingCommit;
use spine_core::RawBoundary;
use spine_core::RecordDigest;
use spine_core::ReplayInput;
use spine_core::SamplingArchiveRecord;
use spine_core::SamplingFinish;
use spine_core::SamplingHandle;
use spine_core::SamplingRuntime;
use spine_core::SamplingTerminal;
use spine_core::SpineCompactBarrierV1;
use spine_core::SpineConfig;
use spine_core::SpineOperationFact;
use spine_core::SpineProjection;
use spine_core::ThreadNamespace;
use spine_core::TokenUsageSample;
use spine_core::ToolUse;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::Arc;

mod archive;
mod replay;
mod session;

pub(crate) use archive::CoordinatorError;
pub(crate) use archive::ReplayMode;
pub(crate) use archive::decode_spine_rollout_item;
use archive::encode_spine_sampling_started;
use archive::encode_spine_transition;
pub(crate) use archive::replay_mode;
pub(crate) use session::SpineSessionAdapter;

pub(crate) type SpineSamplingAttempt = SamplingHandle;
pub(crate) type SharedSpineCoordinator = Arc<std::sync::Mutex<Option<CodexSpineCoordinator>>>;

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
}

pub(crate) struct CodexSpineCoordinator {
    pub(crate) runtime: SamplingRuntime,
    runtime_config: SpineConfig,
    next_boundary: u64,
    pending_calls: HashMap<String, ToolUse>,
    source_items: BTreeMap<spine_core::SourceCellId, ResponseItem>,
    spawn_enabled: bool,
    node_prompt: String,
    pub(crate) durability_fault: Option<String>,
    usage_samples: Vec<TokenUsageSample>,
    context_window_samples: Vec<ContextWindowSample>,
    user_messages: Vec<SpinetreeUserMessageProjectionEntry>,
}

impl CodexSpineCoordinator {
    pub(crate) fn new(
        thread: impl Into<String>,
        config: SpineConfig,
    ) -> Result<Self, CoordinatorError> {
        let thread = ThreadNamespace::parse(thread.into())
            .map_err(|error| CoordinatorError::Identity(error.to_string()))?;
        let spawn_enabled = config.is_enabled(spine_core::Feature::Spawn);
        let node_prompt = config.node_prompt().unwrap_or_default().to_string();
        let runtime = SamplingRuntime::new(thread, ContextEpoch::ZERO, config.clone())?;
        Ok(Self {
            runtime,
            runtime_config: config,
            next_boundary: 0,
            pending_calls: HashMap::new(),
            source_items: BTreeMap::new(),
            spawn_enabled,
            node_prompt,
            durability_fault: None,
            usage_samples: Vec::new(),
            context_window_samples: Vec::new(),
            user_messages: Vec::new(),
        })
    }

    pub(crate) fn observe_response_items(
        &mut self,
        items: &[ResponseItem],
    ) -> Result<PreparedCodexContextPlan, CoordinatorError> {
        self.require_healthy()?;
        let mut characters = Vec::with_capacity(items.len());
        let mut projected_items = Vec::with_capacity(items.len());
        for item in items {
            let boundary = RawBoundary(self.next_boundary);
            self.next_boundary = self.next_boundary.saturating_add(1);
            let (character, projected) = response_item_to_char_and_source(
                item,
                boundary,
                &mut self.pending_calls,
                self.spawn_enabled,
            );
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
    ) -> Result<codex_protocol::protocol::SpineSamplingStartedItem, CoordinatorError> {
        let encoded = serde_json::to_vec(prompt)
            .map_err(|error| CoordinatorError::Codec(error.to_string()))?;
        let record = self
            .runtime
            .sampling_started_record(attempt, RecordDigest::digest(&encoded))?;
        encode_spine_sampling_started(&record)
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

    pub(crate) fn current_input_tokens(&self) -> Option<i64> {
        self.runtime
            .current_input_tokens()
            .and_then(|tokens| i64::try_from(tokens).ok())
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
        let output = self.runtime.install_prepared(prepared)?;
        Ok(InstalledCanonicalCommit {
            context,
            projection: output.projection,
        })
    }

    pub(crate) fn publish_canonical_sampling(&mut self, commit: &InstalledCanonicalCommit) {
        self.user_messages = commit.context.user_messages.clone();
    }

    pub(crate) fn compact_live(
        &mut self,
        replacement_items: &[ResponseItem],
    ) -> Result<(), CoordinatorError> {
        self.require_healthy()?;
        let epoch = self.runtime.epoch();
        let next_epoch = epoch.checked_next().ok_or_else(|| {
            CoordinatorError::Identity("Spine context epoch is exhausted".to_string())
        })?;
        let boundary = RawBoundary(self.next_boundary);
        let replacement_boundaries = (0..replacement_items.len())
            .scan(boundary.0, |next, _| {
                *next = next.saturating_add(1);
                Some(RawBoundary(*next))
            })
            .collect::<Vec<_>>();
        let barrier = SpineCompactBarrierV1::new(
            self.runtime.thread().clone(),
            epoch,
            next_epoch,
            boundary,
            replacement_boundaries.clone(),
        )
        .map_err(|error| CoordinatorError::Archive(error.to_string()))?;
        self.runtime.compact(barrier)?;
        self.next_boundary = replacement_boundaries.last().map_or_else(
            || boundary.0.saturating_add(1),
            |boundary| boundary.0.saturating_add(1),
        );
        self.pending_calls.clear();
        self.source_items = self
            .runtime
            .source_snapshot()
            .cells()
            .iter()
            .map(|cell| cell.id.clone())
            .zip(replacement_items.iter().cloned())
            .collect();
        self.user_messages.clear();
        Ok(())
    }

    pub(crate) fn observe_token_count(
        &mut self,
        event: &codex_protocol::protocol::TokenCountEvent,
    ) {
        let Some(info) = event.info.as_ref() else {
            return;
        };
        if let Some(model_context_window) = info.model_context_window {
            self.record_context_window(model_context_window);
        }
        self.usage_samples.push(TokenUsageSample {
            boundary: RawBoundary(self.next_boundary),
            input_tokens: info.last_token_usage.input_tokens,
        });
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
        origin: spine_core::ExecutionOrigin,
        operation: SpineOperationFact,
    ) -> Result<(), CoordinatorError> {
        self.require_healthy()?;
        Ok(self.runtime.stage_execution(key, origin, operation)?)
    }

    pub(crate) fn validate_control(&self, tool: spine_core::SpineTool) -> Result<(), String> {
        self.require_healthy().map_err(|error| error.to_string())?;
        if matches!(
            tool,
            spine_core::SpineTool::Close | spine_core::SpineTool::Next
        ) {
            let projection = self.runtime.projection();
            let cursor = projection
                .nodes
                .iter()
                .find(|node| node.id == projection.cursor)
                .ok_or_else(|| "Spine cursor is missing from the derived tree".to_string())?;
            if cursor.kind == spine_core::NodeKind::RootEpoch {
                return Err("no open Spine node is available to close".to_string());
            }
        }
        Ok(())
    }

    pub(crate) fn prepare_trim(
        &self,
        current_call_id: &str,
        request: &spine_core::TrimRequest,
    ) -> Result<SpineOperationFact, String> {
        if self
            .pending_calls
            .get(current_call_id)
            .is_none_or(|call| call.name != "spine.trim")
        {
            return Err(
                "spine.trim failed: current toolcall is unavailable; do not retry".to_string(),
            );
        }
        self.require_healthy().map_err(|error| error.to_string())?;
        self.runtime
            .validated_trim_fact(request)
            .map_err(|error| error.to_string())
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
