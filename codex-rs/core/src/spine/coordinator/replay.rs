use super::super::context_handler::SamplingReplies;
use super::super::context_handler::response_item_to_char_and_source;
use super::*;

impl CodexSpineCoordinator {
    pub(crate) fn replay_canonical(
        &mut self,
        effective: &[(usize, &RolloutItem)],
        native_history: &[ResponseItemEnvelope],
        replay_thread: ThreadNamespace,
        records: Vec<SamplingArchiveRecord>,
    ) -> Result<InstalledCanonicalCommit, CoordinatorError> {
        self.require_healthy()?;
        let mut records = records.into_iter();
        let mut inputs = Vec::new();
        let mut epoch = ContextEpoch::ZERO;
        let mut projected_source_items = Vec::new();
        let mut next_boundary = 0u64;
        let mut context_window_samples = Vec::new();
        let mut replay_record_thread = replay_thread.clone();
        let continuation_thread = self.runtime.thread().clone();

        let first_started =
            effective
                .iter()
                .enumerate()
                .find_map(|(index, (_, item))| match item {
                    RolloutItem::SpineSamplingStarted(started) => Some((index, started)),
                    _ => None,
                });
        let start_index = if let Some((index, started)) = first_started
            && let Some(seed) = &started.replay_seed
        {
            let seed: Vec<ReplaySeedItem> =
                serde_json::from_value(seed.clone()).map_err(|error| {
                    CoordinatorError::Replay(format!("invalid initialization seed: {error}"))
                })?;
            for item in seed {
                match item {
                    ReplaySeedItem::Source { boundary, item } => {
                        let item = super::archive::seed_response_item(item)?;
                        let (character, projected) =
                            response_item_to_char_and_source(&item, RawBoundary(boundary))
                                .map_err(CoordinatorError::Replay)?;
                        inputs.push(ReplayInput::Source(character));
                        projected_source_items.push(projected);
                        next_boundary = boundary.saturating_add(1);
                    }
                    ReplaySeedItem::Compact {
                        barrier,
                        replacement,
                    } => {
                        epoch = barrier.next_epoch;
                        next_boundary = barrier
                            .replacement_boundaries
                            .last()
                            .map_or(barrier.boundary.0, |boundary| boundary.0)
                            .saturating_add(1);
                        inputs.push(ReplayInput::Compact(barrier));
                        projected_source_items = replacement
                            .into_iter()
                            .map(super::archive::seed_response_item)
                            .collect::<Result<Vec<_>, _>>()?;
                    }
                    ReplaySeedItem::Usage {
                        boundary,
                        input_tokens,
                        model_context_window,
                    } => {
                        inputs.push(ReplayInput::Usage(TokenUsageSample {
                            boundary: RawBoundary(boundary),
                            input_tokens,
                        }));
                        if let Some(model_context_window) = model_context_window {
                            context_window_samples.push(ContextWindowSample {
                                boundary: RawBoundary(boundary),
                                model_context_window,
                            });
                        }
                    }
                }
            }
            index
        } else {
            0
        };
        let mut replies: Option<SamplingReplies<ResponseItemEnvelope>> = None;
        let settle = |settled: Vec<(SpineChar, ResponseItemEnvelope)>,
                      inputs: &mut Vec<ReplayInput>,
                      projected_source_items: &mut Vec<ResponseItemEnvelope>| {
            for (character, projected) in settled {
                projected_source_items.push(projected);
                inputs.push(ReplayInput::Source(character));
            }
        };
        for (_, item) in &effective[start_index..] {
            let source = match item {
                RolloutItem::ResponseItem(item) => Some(item.clone()),
                RolloutItem::InterAgentCommunication(communication) => {
                    Some(communication.to_model_input_item().into())
                }
                _ => None,
            };
            if let Some(item) = source {
                let boundary = RawBoundary(next_boundary);
                let (character, projected) = response_item_to_char_and_source(&item, boundary)
                    .map_err(CoordinatorError::Replay)?;
                next_boundary = boundary.0.saturating_add(1);
                let settled = match &mut replies {
                    Some(replies) => replies.observe(character, projected),
                    None => vec![(character, projected)],
                };
                settle(settled, &mut inputs, &mut projected_source_items);
                continue;
            }
            match item {
                RolloutItem::SpineSamplingStarted(_) | RolloutItem::SpineTransition(_) => {
                    if decode_spine_rollout_item(item)?.is_none() {
                        continue;
                    }
                    // The same hold as live: a sampling's assistant messages
                    // settle when its commit says how it ended, or as plain
                    // assistant messages when the next sampling starts first.
                    let completed = matches!(item, RolloutItem::SpineTransition(transition) if transition.completed);
                    if let Some(held) = replies.take() {
                        settle(
                            held.finish(completed),
                            &mut inputs,
                            &mut projected_source_items,
                        );
                    }
                    if matches!(item, RolloutItem::SpineSamplingStarted(_)) {
                        replies = Some(SamplingReplies::default());
                    }
                    let record = records.next().ok_or_else(|| {
                        CoordinatorError::Replay(
                            "canonical replay record order diverged".to_string(),
                        )
                    })?;
                    replay_record_thread = match &record {
                        SamplingArchiveRecord::SamplingStarted(started) => {
                            started.attempt_id.thread().clone()
                        }
                        SamplingArchiveRecord::SamplingCommit(commit) => {
                            commit.commit_id.thread().clone()
                        }
                    };
                    inputs.push(ReplayInput::Archive(record));
                }
                RolloutItem::Compacted(compacted) => {
                    let boundary = RawBoundary(next_boundary);
                    let replacement_items =
                        compacted.replacement_history.clone().unwrap_or_else(|| {
                            vec![
                                ResponseItem::Message {
                                    id: None,
                                    role: "assistant".to_string(),
                                    content: vec![
                                        codex_protocol::models::ContentItem::OutputText {
                                            text: compacted.message.clone(),
                                        },
                                    ],
                                    phase: None,
                                    internal_chat_message_metadata_passthrough: None,
                                }
                                .into(),
                            ]
                        });
                    let replacement_boundaries = (0..replacement_items.len())
                        .scan(boundary.0, |next, _| {
                            *next = next.saturating_add(1);
                            Some(RawBoundary(*next))
                        })
                        .collect::<Vec<_>>();
                    let next_epoch = epoch.checked_next().ok_or_else(|| {
                        CoordinatorError::Replay("Spine replay epoch exhausted".to_string())
                    })?;
                    inputs.push(ReplayInput::Compact(
                        SpineCompactBarrierV1::new(
                            replay_record_thread.clone(),
                            epoch,
                            next_epoch,
                            boundary,
                            replacement_boundaries.clone(),
                        )
                        .map_err(|error| CoordinatorError::Replay(error.to_string()))?,
                    ));
                    epoch = next_epoch;
                    projected_source_items = replacement_items;
                    next_boundary = replacement_boundaries.last().map_or_else(
                        || boundary.0.saturating_add(1),
                        |boundary| boundary.0.saturating_add(1),
                    );
                }
                RolloutItem::EventMsg(codex_protocol::protocol::EventMsg::TokenCount(event)) => {
                    if let Some(info) = event.info.as_ref() {
                        if let Some(model_context_window) = info.model_context_window {
                            context_window_samples.push(ContextWindowSample {
                                boundary: RawBoundary(next_boundary),
                                model_context_window,
                            });
                        }
                        inputs.push(ReplayInput::Usage(TokenUsageSample {
                            boundary: RawBoundary(next_boundary),
                            input_tokens: info.last_token_usage.input_tokens,
                        }));
                    }
                }
                RolloutItem::SessionMeta(_)
                | RolloutItem::ResponseItem(_)
                | RolloutItem::InterAgentCommunication(_)
                | RolloutItem::InterAgentCommunicationMetadata { .. }
                | RolloutItem::TurnContext(_)
                | RolloutItem::TokenUsageRecord(_)
                | RolloutItem::SecurityRiskScore(_)
                | RolloutItem::RealtimeItem(_)
                | RolloutItem::WorldState(_)
                | RolloutItem::EventMsg(_) => {}
            }
        }

        if let Some(held) = replies.take() {
            settle(held.finish(false), &mut inputs, &mut projected_source_items);
        }
        let entered_arrivals = inputs
            .iter()
            .filter_map(|input| match input {
                ReplayInput::Source(SpineChar::Arrival { arrival, .. }) => {
                    Some((arrival.producer.clone(), arrival.execution_ref.clone()))
                }
                _ => None,
            })
            .collect::<BTreeMap<_, _>>();
        let prepared = CanonicalReplay::new(replay_thread)
            .map_err(|error| CoordinatorError::Replay(error.to_string()))?
            .with_runtime_config(self.runtime_config.clone())
            .map_err(|error| CoordinatorError::Replay(error.to_string()))?
            .prepare(inputs)
            .map_err(|error| CoordinatorError::Replay(error.to_string()))?;
        let projection = prepared.projection.clone();
        let usage_samples = prepared.usage_samples.clone();
        let live_plan = prepared.live_plan.clone();
        let mut runtime = prepared.into_runtime();
        let snapshot = runtime.source_snapshot();
        if snapshot.cells().len() != projected_source_items.len() {
            return Err(CoordinatorError::Replay(
                "canonical source identities diverged from native source items".to_string(),
            ));
        }
        let source_items = snapshot
            .cells()
            .iter()
            .map(|cell| cell.id.clone())
            .zip(projected_source_items.iter().cloned())
            .collect::<BTreeMap<_, _>>();
        let context = match live_plan {
            Some(plan) => {
                let node_context_costs = runtime.node_context_costs(&context_window_samples);
                prepare_codex_context_plan(
                    &plan,
                    &runtime.source_snapshot(),
                    &source_items,
                    &node_context_costs,
                    &self.node_prompt,
                )?
            }
            None => PreparedCodexContextPlan {
                items: native_history.to_vec(),
                user_messages: Vec::new(),
            },
        };
        runtime.continue_in_namespace(continuation_thread)?;
        let source_items = runtime
            .source_snapshot()
            .cells()
            .iter()
            .map(|cell| cell.id.clone())
            .zip(projected_source_items)
            .collect();

        self.replay_seed = None;
        self.runtime = runtime;
        self.next_boundary = next_boundary;
        self.source_items = source_items;
        self.usage_samples = usage_samples;
        self.context_window_samples = context_window_samples;
        self.user_messages = context.user_messages.clone();
        self.entered_arrivals = entered_arrivals;
        self.source_revision = self.source_revision.wrapping_add(1);
        Ok(InstalledCanonicalCommit {
            context,
            projection,
            settled_spawn_call_ids: Vec::new(),
        })
    }
}
