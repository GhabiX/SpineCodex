use super::context_handler::CodexContextHandler;
use super::context_handler::response_item_to_char;
use super::context_handler::response_item_to_char_and_source;
use super::coordinator::ReplayMode;
use super::coordinator::SharedSpineCoordinator;
use super::coordinator::replay_mode;
use super::session_config::SpineSessionConfig;
use crate::context_manager::ContextManager;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::TokenCountEvent;
use spine_core::RawBoundary;
use spine_core::SpineContextRuntime;
use spine_core::SpineRecoveryInput;
use spine_core::SpineSignal;
use spine_core::ToolUse;
use std::collections::HashMap;

pub(crate) struct SessionSpineRuntime {
    trim_runtime: Option<SpineContextRuntime<CodexContextHandler>>,
    model_context: ContextManager,
    next_boundary: u64,
    pending_calls: HashMap<String, ToolUse>,
    trim_enabled: bool,
    coordinator: SharedSpineCoordinator,
}

impl SessionSpineRuntime {
    pub(crate) fn new(
        configuration: &SpineSessionConfig,
        coordinator: SharedSpineCoordinator,
    ) -> Option<Self> {
        let jit_enabled = configuration.jit_enabled();
        let trim_enabled = configuration.trim_enabled();
        let enabled = jit_enabled || trim_enabled;
        enabled.then(|| {
            let config = configuration.sdk().clone();
            let trim_runtime = (trim_enabled && !jit_enabled).then(|| {
                let handler = CodexContextHandler::new(&config);
                SpineContextRuntime::new(config.clone(), handler)
                    .expect("validated session Spine configuration must initialize")
            });
            Self {
                trim_runtime,
                model_context: ContextManager::new(),
                next_boundary: 0,
                pending_calls: HashMap::new(),
                trim_enabled,
                coordinator,
            }
        })
    }

    fn with_coordinator<R>(
        &self,
        f: impl FnOnce(&mut super::coordinator::CodexSpineCoordinator) -> R,
    ) -> Option<R> {
        self.coordinator
            .lock()
            .unwrap_or_else(|_| panic!("Spine coordinator mutex must not be poisoned"))
            .as_mut()
            .map(f)
    }

    pub(crate) fn append_response_items(&mut self, items: &[ResponseItem]) -> Result<(), String> {
        if let Some(result) =
            self.with_coordinator(|coordinator| coordinator.observe_response_items(items))
        {
            match result {
                Ok(context) => self.model_context.replace(context.items),
                Err(error) => {
                    let reason = error.to_string();
                    self.with_coordinator(|coordinator| {
                        coordinator.latch_durability_fault(reason.clone());
                    });
                    return Err(reason);
                }
            }
            return Ok(());
        }
        let Some(runtime) = self.trim_runtime.as_mut() else {
            return Ok(());
        };
        let mut sources = Vec::with_capacity(items.len());
        let chars = items
            .iter()
            .map(|item| {
                let boundary = RawBoundary(self.next_boundary);
                self.next_boundary = self.next_boundary.saturating_add(1);
                let (character, source) = response_item_to_char_and_source(
                    item,
                    boundary,
                    &mut self.pending_calls,
                    runtime.handler().spawn_enabled(),
                );
                sources.push((boundary, source));
                character
            })
            .collect::<Vec<_>>();
        runtime.handler_mut().stage_sources(sources);
        let mut candidate = self.model_context.clone();
        let mut appended = candidate.raw_items().to_vec();
        appended.extend_from_slice(items);
        candidate.replace(appended);
        runtime
            .append(chars, &mut candidate)
            .map_err(|error| error.to_string())?;
        self.model_context = candidate;
        Ok(())
    }

    pub(crate) fn model_context(&self) -> ContextManager {
        self.model_context.clone()
    }

    pub(crate) fn observe_token_count(&mut self, event: TokenCountEvent) {
        if self
            .with_coordinator(|coordinator| coordinator.observe_token_count(&event))
            .is_some()
        {
            return;
        }
        if let Some(usage) = event.info.map(|info| info.last_token_usage)
            && let Some(runtime) = self.trim_runtime.as_mut()
        {
            runtime.observe_usage(spine_core::TokenUsageSample {
                boundary: RawBoundary(self.next_boundary),
                input_tokens: usage.input_tokens,
            });
        }
    }

    pub(crate) fn current_input_tokens(&self) -> Option<i64> {
        self.with_coordinator(|coordinator| coordinator.current_input_tokens())
            .flatten()
    }

    pub(crate) fn compact_live(
        &mut self,
        replacement_items: &[ResponseItem],
    ) -> Result<(), String> {
        if let Some(result) =
            self.with_coordinator(|coordinator| coordinator.compact_live(replacement_items))
        {
            match result {
                Ok(()) => {
                    self.model_context.replace(replacement_items.to_vec());
                    return Ok(());
                }
                Err(error) => {
                    let reason = error.to_string();
                    self.with_coordinator(|coordinator| {
                        coordinator.latch_durability_fault(reason.clone());
                    });
                    return Err(reason);
                }
            }
        }
        let Some(runtime) = self.trim_runtime.as_mut() else {
            return Ok(());
        };
        let compact_boundary = RawBoundary(self.next_boundary);
        self.next_boundary = self.next_boundary.saturating_add(1);
        runtime.handler_mut().reset_sources();
        self.pending_calls.clear();
        let mut candidate = ContextManager::new();
        candidate.replace(replacement_items.to_vec());
        let mut sources = Vec::with_capacity(replacement_items.len());
        let chars = replacement_items
            .iter()
            .map(|item| {
                let boundary = RawBoundary(self.next_boundary);
                self.next_boundary = self.next_boundary.saturating_add(1);
                sources.push((boundary, item.clone()));
                spine_core::SpineChar::Opaque { boundary }
            })
            .collect::<Vec<_>>();
        runtime.handler_mut().stage_sources(sources);
        runtime
            .compact_live(compact_boundary, chars, &mut candidate)
            .map_err(|error| error.to_string())?;
        self.model_context = candidate;
        Ok(())
    }

    pub(crate) fn publish_canonical_compact(&mut self) {
        self.with_coordinator(super::coordinator::CodexSpineCoordinator::publish_canonical_compact);
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
                if let Some(result) = self.with_coordinator(|coordinator| {
                    coordinator.observe_response_items(raw_history.raw_items())
                }) {
                    match result {
                        Ok(context) => candidate.replace(context.items),
                        Err(error) => {
                            let reason = error.to_string();
                            self.with_coordinator(|coordinator| {
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
                let result = self.with_coordinator(|coordinator| {
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
                self.with_coordinator(|coordinator| {
                    coordinator.latch_durability_fault(reason.clone());
                });
                return Err(reason);
            }
        }
        let last_compact = effective
            .iter()
            .rposition(|(_, item)| matches!(item, RolloutItem::Compacted(_)));
        self.pending_calls.clear();
        let Some(runtime) = self.trim_runtime.as_mut() else {
            return Ok(());
        };
        let mut archived = Vec::new();
        let mut replay_boundary = 0u64;
        let mut compact_boundary = None;
        for (index, (ordinal, item)) in effective.iter().copied().enumerate() {
            let archived_context = last_compact.is_some_and(|last| index <= last);
            let archived_source = match item {
                RolloutItem::ResponseItem(item) if archived_context => Some((*item).clone()),
                RolloutItem::InterAgentCommunication(communication) if archived_context => {
                    Some(communication.to_model_input_item())
                }
                _ => None,
            };
            if let Some(item) = archived_source {
                archived.push(SpineRecoveryInput::Char(response_item_to_char(
                    &item,
                    RawBoundary(replay_boundary),
                    &mut self.pending_calls,
                    runtime.handler().spawn_enabled(),
                )));
                replay_boundary = replay_boundary.saturating_add(1);
                continue;
            }
            match item {
                RolloutItem::Compacted(_) if archived_context => {
                    compact_boundary = Some(replay_boundary);
                    archived.push(SpineRecoveryInput::Signal(SpineSignal::Compact {
                        boundary: RawBoundary(replay_boundary),
                    }));
                    replay_boundary = replay_boundary.saturating_add(1);
                }
                RolloutItem::EventMsg(EventMsg::TokenCount(event)) => {
                    if let Some(usage) = event.info.as_ref().map(|info| &info.last_token_usage) {
                        archived.push(SpineRecoveryInput::Signal(SpineSignal::Usage(
                            spine_core::TokenUsageSample {
                                boundary: RawBoundary(ordinal as u64),
                                input_tokens: usage.input_tokens,
                            },
                        )));
                    }
                }
                _ => {}
            }
        }
        runtime.handler_mut().reset_sources();
        self.pending_calls.clear();
        let postcompact_source_count = last_compact.map_or(0, |index| {
            effective
                .iter()
                .skip(index + 1)
                .filter(|(_, item)| {
                    matches!(
                        item,
                        RolloutItem::ResponseItem(_) | RolloutItem::InterAgentCommunication(_)
                    )
                })
                .count()
        });
        let replacement_len = last_compact
            .and_then(|index| match effective[index].1 {
                RolloutItem::Compacted(compacted) => compacted
                    .replacement_history
                    .as_ref()
                    .map(|items| {
                        if raw_history.raw_items().starts_with(items) {
                            items.len()
                        } else {
                            Default::default()
                        }
                    })
                    .or_else(|| {
                        Some(
                            raw_history
                                .raw_items()
                                .len()
                                .saturating_sub(postcompact_source_count),
                        )
                    }),
                _ => None,
            })
            .unwrap_or_default();
        self.next_boundary = compact_boundary.map_or(0, |boundary| boundary + 1);
        let mut sources = Vec::with_capacity(raw_history.raw_items().len());
        let mut chars = Vec::with_capacity(raw_history.raw_items().len());
        for (index, item) in raw_history.raw_items().iter().enumerate() {
            if compact_boundary.is_some_and(|_| index < replacement_len) {
                let boundary = RawBoundary(self.next_boundary);
                self.next_boundary = self.next_boundary.saturating_add(1);
                sources.push((boundary, item.clone()));
                chars.push(spine_core::SpineChar::Opaque { boundary });
                continue;
            }
            let boundary = RawBoundary(self.next_boundary);
            self.next_boundary = self.next_boundary.saturating_add(1);
            let (character, source) = response_item_to_char_and_source(
                item,
                boundary,
                &mut self.pending_calls,
                runtime.handler().spawn_enabled(),
            );
            sources.push((boundary, source));
            chars.push(character);
        }
        runtime.handler_mut().stage_sources(sources);
        runtime
            .recover(archived, chars, &mut candidate)
            .map_err(|error| error.to_string())?;
        self.model_context = candidate;
        Ok(())
    }

    pub(crate) fn replace_last_turn_images(&mut self, placeholder: &str) {
        if let Some(runtime) = self.trim_runtime.as_mut() {
            runtime.handler_mut().replace_last_turn_images(placeholder);
        }
    }

    pub(crate) fn install_model_context(&mut self, items: Vec<ResponseItem>) {
        self.model_context.replace(items);
    }

    pub(crate) fn validate_trim(
        &self,
        current_call_id: &str,
        request: &spine_core::TrimRequest,
    ) -> Result<(), String> {
        if !self.trim_enabled {
            return Err("Spine trim is not enabled for this session".to_string());
        }
        if self
            .pending_calls
            .get(current_call_id)
            .is_none_or(|call| call.name != "spine.trim")
        {
            return Err(
                "spine.trim failed: current toolcall is unavailable; do not retry".to_string(),
            );
        }
        self.trim_projection()?.validate(request)
    }

    pub(crate) fn validate_control(&self, tool: spine_core::SpineTool) -> Result<(), String> {
        self.with_coordinator(|coordinator| coordinator.validate_control(tool))
            .unwrap_or_else(|| Err("Spine JIT is not enabled for this session".to_string()))
    }

    fn trim_projection(&self) -> Result<&spine_core::TrimProjection, String> {
        if !self.trim_enabled {
            return Err("Spine trim is not enabled for this session".to_string());
        }
        self.trim_runtime
            .as_ref()
            .ok_or_else(|| "Spine trim runtime is unavailable".to_string())?
            .projection()
            .trim_projection()
            .ok_or_else(|| "Spine trim runtime is unavailable".to_string())
    }
}
