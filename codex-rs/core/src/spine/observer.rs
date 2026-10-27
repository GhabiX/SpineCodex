use super::memory_projection::SpinetreeMemoryProjection;
use super::memory_projection::SpinetreeMemoryProjectionEntry;
use super::memory_projection::SpinetreeUserMessageProjectionEntry;
use async_channel::Sender;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::SpineTreeNodeSnapshot;
use codex_protocol::protocol::SpineTreeUpdateEvent;
use codex_protocol::spine_tree::SpineNodeContextPressureProblem;
use codex_protocol::spine_tree::SpineNodeContextPressureSnapshot;
use codex_protocol::spine_tree::SpineTreeNodeKind;
use codex_protocol::spine_tree::SpineTreeNodeStatus;
use spine_core::host::ContextPressureProblem;
use spine_core::host::NodeKind;
use spine_core::host::NodeStatus;
use tokio::sync::watch;
use tracing::warn;

#[derive(Clone, Debug, PartialEq, Eq)]
struct CodexSpineMemoryProjection {
    entries: Vec<SpinetreeMemoryProjectionEntry>,
    user_messages: Vec<SpinetreeUserMessageProjectionEntry>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct CodexSpineObserverHandler {
    tx_event: Option<Sender<Event>>,
    fallback_event_id: String,
    memory_projection_tx: Option<watch::Sender<Option<CodexSpineMemoryProjection>>>,
    jit_enabled: bool,
}

impl CodexSpineObserverHandler {
    pub(crate) fn new(
        tx_event: Sender<Event>,
        fallback_event_id: String,
        memory_projection: Option<SpinetreeMemoryProjection>,
        jit_enabled: bool,
    ) -> Self {
        Self {
            tx_event: Some(tx_event),
            fallback_event_id,
            memory_projection_tx: memory_projection.map(start_memory_projection_worker),
            jit_enabled,
        }
    }

    pub(crate) fn publish_committed(
        &mut self,
        projection: &spine_core::host::SpineProjection,
        usage_samples: &[spine_core::host::TokenUsageSample],
        event_id: Option<&str>,
        user_messages: Vec<SpinetreeUserMessageProjectionEntry>,
        settled_spawn_call_ids: &[String],
    ) {
        if !self.jit_enabled {
            return;
        }
        self.publish_tree(projection, usage_samples, event_id, settled_spawn_call_ids);
        self.publish_memory(projection, user_messages);
    }

    pub(crate) fn publish_usage(
        &mut self,
        projection: &spine_core::host::SpineProjection,
        usage_samples: &[spine_core::host::TokenUsageSample],
        event_id: &str,
    ) {
        if self.jit_enabled {
            self.publish_tree(projection, usage_samples, Some(event_id), &[]);
        }
    }

    fn publish_tree(
        &self,
        projection: &spine_core::host::SpineProjection,
        usage_samples: &[spine_core::host::TokenUsageSample],
        event_id: Option<&str>,
        settled_spawn_call_ids: &[String],
    ) {
        let Some(tx_event) = &self.tx_event else {
            return;
        };
        let event = Event {
            id: event_id.unwrap_or(&self.fallback_event_id).to_string(),
            msg: EventMsg::SpineTreeUpdate(tree_update_from_parts(
                projection,
                usage_samples,
                settled_spawn_call_ids,
            )),
        };
        if let Err(err) = tx_event.try_send(event) {
            warn!("failed to publish Spine tree update: {err}");
        }
    }

    fn publish_memory(
        &self,
        projection: &spine_core::host::SpineProjection,
        user_messages: Vec<SpinetreeUserMessageProjectionEntry>,
    ) {
        if let Some(tx) = &self.memory_projection_tx {
            tx.send_replace(Some(CodexSpineMemoryProjection {
                entries: super::closed_memory_projection_entries(projection),
                user_messages,
            }));
        }
    }
}

fn start_memory_projection_worker(
    projection: SpinetreeMemoryProjection,
) -> watch::Sender<Option<CodexSpineMemoryProjection>> {
    let (tx, mut rx) = watch::channel::<Option<CodexSpineMemoryProjection>>(None);
    let _worker = tokio::spawn(async move {
        while rx.changed().await.is_ok() {
            let Some(memory) = rx.borrow_and_update().clone() else {
                continue;
            };
            let projection = projection.clone();
            match tokio::task::spawn_blocking(move || {
                projection.persist(&memory.entries, &memory.user_messages)
            })
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(err)) => warn!("failed to publish Spine memory projection: {err:#}"),
                Err(err) => warn!("Spine memory projection task failed: {err}"),
            }
        }
    });
    tx
}

pub(crate) fn tree_update_from_parts(
    projection: &spine_core::host::SpineProjection,
    usage_samples: &[spine_core::host::TokenUsageSample],
    settled_spawn_call_ids: &[String],
) -> SpineTreeUpdateEvent {
    let snapshot = spine_core::host::tree_snapshot(projection, usage_samples);
    SpineTreeUpdateEvent {
        snapshot_seq: snapshot.last_boundary.map_or(0, |boundary| boundary.0),
        active_node_id: snapshot.cursor.to_string(),
        nodes: snapshot
            .nodes
            .into_iter()
            .map(|node| SpineTreeNodeSnapshot {
                node_id: node.id.to_string(),
                parent_id: node.parent.map(|id| id.to_string()),
                kind: match node.kind {
                    NodeKind::RootEpoch => SpineTreeNodeKind::RootEpoch,
                    NodeKind::Task => SpineTreeNodeKind::Task,
                },
                status: match node.status {
                    NodeStatus::Live => SpineTreeNodeStatus::Live,
                    NodeStatus::Opened => SpineTreeNodeStatus::Opened,
                    NodeStatus::Closed => SpineTreeNodeStatus::Closed,
                    NodeStatus::Compacted => SpineTreeNodeStatus::Compacted,
                },
                summary: node.summary,
                memory_summary: node.memory_summary,
                spawn_outcome: node.spawn_outcome.map(|outcome| match outcome {
                    spine_core::host::SpawnOutcome::Completed => {
                        codex_protocol::spine_tree::SpineSpawnOutcome::Completed
                    }
                    spine_core::host::SpawnOutcome::Errored => {
                        codex_protocol::spine_tree::SpineSpawnOutcome::Errored
                    }
                    spine_core::host::SpawnOutcome::Aborted => {
                        codex_protocol::spine_tree::SpineSpawnOutcome::Aborted
                    }
                }),
                start: node.start.0,
                end: node.end.map(|boundary| boundary.0),
                context_pressure: node
                    .pressure
                    .map(|pressure| SpineNodeContextPressureSnapshot {
                        open_input_tokens: pressure.open_input_tokens,
                        current_input_tokens: pressure.current_input_tokens,
                        context_tokens: pressure.context_tokens,
                        problem: pressure.problem.map(|problem| match problem {
                            ContextPressureProblem::MissingCurrentUsage => {
                                SpineNodeContextPressureProblem::MissingCurrentUsage
                            }
                            ContextPressureProblem::MissingOpenContextBaseline => {
                                SpineNodeContextPressureProblem::MissingOpenContextBaseline
                            }
                            ContextPressureProblem::CoordinateMismatch => {
                                SpineNodeContextPressureProblem::CoordinateMismatch
                            }
                        }),
                    }),
            })
            .collect(),
        settled_spawn_call_ids: settled_spawn_call_ids.to_vec(),
        settled_spawn_thread_ids: projection
            .nodes
            .iter()
            .flat_map(|node| {
                node.memory.iter().flatten().filter_map(|slot| match slot {
                    spine_core::host::MemorySlot::SpawnEvidence {
                        owner_node,
                        execution_ref: Some(execution_ref),
                        ..
                    } if owner_node == &node.id => {
                        codex_protocol::ThreadId::from_string(execution_ref).ok()
                    }
                    _ => None,
                })
            })
            .collect(),
    }
}
