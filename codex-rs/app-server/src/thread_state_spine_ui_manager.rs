use super::*;
use crate::spine_ui::SpineUiState;
use crate::spine_ui::listener::Command;
use codex_protocol::protocol::SpineSpawnProgressEvent;

pub(in crate::thread_state) fn remove_thread_routes(
    state: &mut ThreadStateManagerInner,
    thread_id: ThreadId,
) {
    state
        .spine_ui
        .parent_by_child
        .retain(|child_thread_id, route| {
            *child_thread_id != thread_id && route.parent_thread_id != thread_id
        });
}

pub(in crate::thread_state) fn clear_routes(state: &mut ThreadStateManagerInner) {
    state.spine_ui.parent_by_child.clear();
}

impl ThreadStateManager {
    #[cfg(test)]
    pub(crate) async fn spine_ui_listener_generation_for_test(&self, thread_id: ThreadId) -> u64 {
        self.state
            .lock()
            .await
            .threads
            .get(&thread_id)
            .map(|entry| entry.spine_ui.listener_generation)
            .unwrap_or_default()
    }

    pub(crate) async fn note_spine_ui_listener_generation(
        &self,
        thread_id: ThreadId,
        listener_generation: u64,
    ) {
        let invalidation_target = {
            let mut state = self.state.lock().await;
            let generation_changed = {
                let entry = state.threads.entry(thread_id).or_default();
                if listener_generation < entry.spine_ui.listener_generation {
                    return;
                }
                let generation_changed = entry.spine_ui.listener_generation != listener_generation;
                if generation_changed {
                    entry.spine_ui.active_turn_id = None;
                    entry.spine_ui.terminal_ack = None;
                }
                entry.spine_ui.listener_generation = listener_generation;
                generation_changed
            };
            if !generation_changed {
                return;
            }
            let stale_active = state
                .spine_ui
                .parent_by_child
                .iter()
                .filter_map(|(child_thread_id, route)| {
                    (route.parent_thread_id == thread_id
                        && route.parent_listener_generation != listener_generation)
                        .then_some(*child_thread_id)
                })
                .collect::<Vec<_>>();
            for child_thread_id in stale_active {
                state.spine_ui.parent_by_child.remove(&child_thread_id);
            }

            let previous = state.spine_ui.parent_by_child.get(&thread_id).cloned();
            previous.and_then(|previous| {
                state.spine_ui.next_route_generation =
                    state.spine_ui.next_route_generation.saturating_add(1);
                let generation = state.spine_ui.next_route_generation;
                let route = state.spine_ui.parent_by_child.get_mut(&thread_id)?;
                route.expected_child_turn_id = None;
                route.generation = generation;
                route.queued_revision = None;
                route.last_forwarded_revision = None;
                Some((
                    previous.parent_thread_id,
                    previous.parent_turn_id,
                    previous.parent_listener_generation,
                    previous.generation,
                ))
            })
        };

        if let Some((parent_thread_id, parent_turn_id, parent_listener_generation, generation)) =
            invalidation_target
        {
            self.invalidate_spine_ui_parent_agent_state(
                parent_thread_id,
                parent_turn_id,
                parent_listener_generation,
                thread_id,
                generation,
            )
            .await;
        }
    }

    pub(crate) async fn note_spine_ui_agent_turn_started(
        &self,
        thread_id: ThreadId,
        listener_generation: u64,
        turn_id: &str,
    ) {
        let mut state = self.state.lock().await;
        let Some(entry) = state.threads.get(&thread_id) else {
            return;
        };
        if entry.spine_ui.listener_generation != listener_generation {
            return;
        }
        let stale_children = state
            .spine_ui
            .parent_by_child
            .iter()
            .filter_map(|(child_thread_id, route)| {
                (route.parent_thread_id == thread_id).then_some(*child_thread_id)
            })
            .collect::<Vec<_>>();
        for child_thread_id in stale_children {
            state.spine_ui.parent_by_child.remove(&child_thread_id);
        }

        let Some(entry) = state.threads.get_mut(&thread_id) else {
            return;
        };
        entry.spine_ui.active_turn_id = Some(turn_id.to_string());
        entry.spine_ui.terminal_ack = None;
        if let Some(route) = state.spine_ui.parent_by_child.get_mut(&thread_id) {
            route.expected_child_turn_id = Some(turn_id.to_string());
        }
    }

    pub(crate) async fn spine_ui_state_for_thread(
        &self,
        thread_id: ThreadId,
    ) -> Option<SpineUiState> {
        let thread_state = self
            .state
            .lock()
            .await
            .threads
            .get(&thread_id)
            .map(|entry| entry.state.clone())?;
        thread_state
            .lock()
            .await
            .current_spine_ui_for_forward()
            .cloned()
    }

    pub(crate) async fn register_spine_ui_spawn_progress(
        &self,
        parent_thread_id: ThreadId,
        parent_listener_generation: u64,
        parent_turn_id: &str,
        progress: &SpineSpawnProgressEvent,
    ) {
        let parent_state = self.thread_state(parent_thread_id).await;
        let baseline_node_ids = Arc::new({
            let parent_state = parent_state.lock().await;
            let Some(node_ids) = parent_state.spine_ui_baseline_node_ids(parent_turn_id) else {
                return;
            };
            node_ids
        });

        let new_child_thread_ids = {
            let mut state = self.state.lock().await;
            if state.threads.get(&parent_thread_id).is_none_or(|entry| {
                entry.spine_ui.listener_generation != parent_listener_generation
            }) {
                return;
            }
            let mut new_child_thread_ids = Vec::new();
            for task in &progress.tasks {
                let expected_child_turn_id = {
                    let entry = state.threads.entry(task.thread_id).or_default();
                    entry.spine_ui.active_turn_id.clone().or_else(|| {
                        entry
                            .spine_ui
                            .terminal_ack
                            .as_ref()
                            .map(|ack| ack.turn_id.clone())
                    })
                };
                if state
                    .spine_ui
                    .parent_by_child
                    .get(&task.thread_id)
                    .is_some_and(|route| {
                        route.parent_thread_id == parent_thread_id
                            && route.parent_turn_id == parent_turn_id
                    })
                {
                    continue;
                }
                if let Some(previous) = state.spine_ui.parent_by_child.remove(&task.thread_id) {
                    let _ = previous;
                }
                state.spine_ui.next_route_generation =
                    state.spine_ui.next_route_generation.saturating_add(1);
                let generation = state.spine_ui.next_route_generation;
                state.spine_ui.parent_by_child.insert(
                    task.thread_id,
                    SpineUiParentRoute {
                        parent_thread_id,
                        parent_turn_id: parent_turn_id.to_string(),
                        expected_child_turn_id,
                        parent_listener_generation,
                        baseline_node_ids: baseline_node_ids.clone(),
                        generation,
                        queued_revision: None,
                        last_forwarded_revision: None,
                    },
                );
                new_child_thread_ids.push(task.thread_id);
            }
            new_child_thread_ids
        };
        for child_thread_id in new_child_thread_ids {
            self.queue_spine_ui_agent_state(child_thread_id).await;
        }
    }

    pub(crate) async fn queue_spine_ui_agent_state(&self, child_thread_id: ThreadId) {
        let (route, child_state) = {
            let state = self.state.lock().await;
            let Some(route) = state
                .spine_ui
                .parent_by_child
                .get(&child_thread_id)
                .cloned()
            else {
                return;
            };
            let Some(child_state) = state
                .threads
                .get(&child_thread_id)
                .map(|entry| entry.state.clone())
            else {
                return;
            };
            (route, child_state)
        };
        let child_state = {
            let child_state = child_state.lock().await;
            let Some(child_state) = child_state.current_spine_ui_for_forward() else {
                return;
            };
            child_state.filtered_for_parent(&route.baseline_node_ids)
        };
        let Some(tx) = self.current_listener_command_tx(route.parent_thread_id) else {
            return;
        };
        let queued_revision = child_state.revision();
        {
            let mut state = self.state.lock().await;
            let Some(current) = state.spine_ui.parent_by_child.get_mut(&child_thread_id) else {
                return;
            };
            if current.generation != route.generation
                || current.queued_revision.is_some()
                || current
                    .last_forwarded_revision
                    .is_some_and(|revision| revision >= queued_revision)
            {
                return;
            }
            current.queued_revision = Some(queued_revision);
        }
        let send_result = tx.send(ThreadListenerCommand::SpineUi(Box::new(
            Command::ForwardSpineUiAgentState {
                child_thread_id,
                parent_turn_id: route.parent_turn_id,
                parent_listener_generation: route.parent_listener_generation,
                generation: route.generation,
                state: Some(child_state),
            },
        )));
        if send_result.is_err() {
            let mut state = self.state.lock().await;
            if let Some(current) = state.spine_ui.parent_by_child.get_mut(&child_thread_id)
                && current.generation == route.generation
                && current.queued_revision == Some(queued_revision)
            {
                current.queued_revision = None;
            }
        }
    }

    pub(crate) async fn complete_spine_ui_agent_state_forward(
        &self,
        child_thread_id: ThreadId,
        parent_thread_id: ThreadId,
        parent_turn_id: &str,
        generation: u64,
        forwarded_revision: u64,
    ) {
        {
            let mut state = self.state.lock().await;
            let Some(route) = state.spine_ui.parent_by_child.get_mut(&child_thread_id) else {
                return;
            };
            if route.parent_thread_id != parent_thread_id
                || route.parent_turn_id != parent_turn_id
                || route.generation != generation
                || route.queued_revision != Some(forwarded_revision)
            {
                return;
            }
            route.queued_revision = None;
            route.last_forwarded_revision = Some(
                route
                    .last_forwarded_revision
                    .unwrap_or_default()
                    .max(forwarded_revision),
            );
        }
        self.queue_spine_ui_agent_state(child_thread_id).await;
    }

    pub(crate) async fn acknowledge_spine_ui_agent_terminal(
        &self,
        child_thread_id: ThreadId,
        listener_generation: u64,
        terminal_turn_id: &str,
    ) {
        let should_forward = {
            let mut state = self.state.lock().await;
            let Some(entry) = state.threads.get_mut(&child_thread_id) else {
                return;
            };
            if entry.spine_ui.listener_generation != listener_generation {
                return;
            }
            let should_cache_terminal = match entry.spine_ui.active_turn_id.as_deref() {
                Some(active_turn_id) if active_turn_id == terminal_turn_id => {
                    entry.spine_ui.active_turn_id = None;
                    true
                }
                Some(_) => false,
                None => entry.spine_ui.terminal_ack.as_ref().is_none_or(|ack| {
                    ack.listener_generation == listener_generation
                        && ack.turn_id == terminal_turn_id
                }),
            };
            if should_cache_terminal {
                entry.spine_ui.terminal_ack = Some(SpineUiTerminalAck {
                    listener_generation,
                    turn_id: terminal_turn_id.to_string(),
                });
            }
            state
                .spine_ui
                .parent_by_child
                .get_mut(&child_thread_id)
                .is_some_and(|route| {
                    if route
                        .expected_child_turn_id
                        .as_deref()
                        .is_some_and(|turn_id| turn_id != terminal_turn_id)
                    {
                        return false;
                    }
                    route.expected_child_turn_id = Some(terminal_turn_id.to_string());
                    true
                })
        };
        if should_forward {
            self.queue_spine_ui_agent_state(child_thread_id).await;
        }
    }

    pub(crate) async fn spine_ui_route_is_current(
        &self,
        child_thread_id: ThreadId,
        parent_thread_id: ThreadId,
        parent_turn_id: &str,
        parent_listener_generation: u64,
        generation: u64,
    ) -> bool {
        self.state
            .lock()
            .await
            .spine_ui
            .parent_by_child
            .get(&child_thread_id)
            .is_some_and(|route| {
                route.parent_thread_id == parent_thread_id
                    && route.parent_turn_id == parent_turn_id
                    && route.parent_listener_generation == parent_listener_generation
                    && route.generation == generation
            })
    }

    pub(crate) async fn clear_spine_ui_parent_routes(
        &self,
        parent_thread_id: ThreadId,
        parent_turn_id: &str,
        parent_listener_generation: u64,
    ) {
        let mut state = self.state.lock().await;
        let child_thread_ids = state
            .spine_ui
            .parent_by_child
            .iter()
            .filter_map(|(child_thread_id, route)| {
                (route.parent_thread_id == parent_thread_id
                    && route.parent_turn_id == parent_turn_id
                    && route.parent_listener_generation == parent_listener_generation)
                    .then_some(*child_thread_id)
            })
            .collect::<Vec<_>>();
        for child_thread_id in child_thread_ids {
            state.spine_ui.parent_by_child.remove(&child_thread_id);
        }
    }

    #[cfg(test)]
    pub(crate) async fn clear_all_spine_ui_routes_for_thread(&self, thread_id: ThreadId) {
        self.clear_spine_ui_routes_for_thread(thread_id, None).await;
    }

    pub(crate) async fn clear_spine_ui_routes_for_listener_exit(
        &self,
        thread_id: ThreadId,
        listener_generation: u64,
        expected_thread_state: &Arc<Mutex<ThreadState>>,
    ) {
        self.clear_spine_ui_routes_for_thread(
            thread_id,
            Some((listener_generation, expected_thread_state)),
        )
        .await;
    }

    async fn clear_spine_ui_routes_for_thread(
        &self,
        thread_id: ThreadId,
        expected_listener: Option<(u64, &Arc<Mutex<ThreadState>>)>,
    ) {
        let invalidation_targets = {
            let mut state = self.state.lock().await;
            if let Some((listener_generation, expected_thread_state)) = expected_listener {
                let Some(entry) = state.threads.get(&thread_id) else {
                    return;
                };
                if !Arc::ptr_eq(&entry.state, expected_thread_state)
                    || entry.spine_ui.listener_generation != listener_generation
                {
                    return;
                }
            }
            if let Some(entry) = state.threads.get_mut(&thread_id) {
                entry.spine_ui.terminal_ack = None;
            }
            let routes = state
                .spine_ui
                .parent_by_child
                .iter()
                .filter_map(|(child_thread_id, route)| {
                    (*child_thread_id == thread_id || route.parent_thread_id == thread_id)
                        .then_some((*child_thread_id, route.clone()))
                })
                .collect::<Vec<_>>();
            let mut targets = Vec::new();
            for (child_thread_id, route) in routes {
                state.spine_ui.parent_by_child.remove(&child_thread_id);
                if child_thread_id == thread_id {
                    targets.push((
                        route.parent_thread_id,
                        route.parent_listener_generation,
                        child_thread_id,
                        route.generation,
                    ));
                }
            }
            targets
        };

        for (parent_thread_id, parent_listener_generation, child_thread_id, generation) in
            invalidation_targets
        {
            let parent_state = {
                let state = self.state.lock().await;
                state
                    .threads
                    .get(&parent_thread_id)
                    .map(|entry| entry.state.clone())
            };
            let Some(parent_state) = parent_state else {
                continue;
            };
            let active_turn_id = {
                let mut parent_state = parent_state.lock().await;
                if parent_state.listener_generation != parent_listener_generation
                    || !parent_state.invalidate_spine_ui_agent_state(child_thread_id, generation)
                {
                    None
                } else {
                    parent_state
                        .active_spine_ui_snapshot()
                        .map(|(turn_id, _)| turn_id)
                }
            };
            if let Some(turn_id) = active_turn_id
                && let Some(tx) = self.current_listener_command_tx(parent_thread_id)
            {
                let _ = tx.send(ThreadListenerCommand::SpineUi(Box::new(
                    Command::EmitSpineUiInvalidation {
                        parent_listener_generation,
                        turn_id,
                    },
                )));
            }
        }
    }

    async fn invalidate_spine_ui_parent_agent_state(
        &self,
        parent_thread_id: ThreadId,
        parent_turn_id: String,
        parent_listener_generation: u64,
        child_thread_id: ThreadId,
        generation: u64,
    ) {
        let parent_state = {
            let state = self.state.lock().await;
            state
                .threads
                .get(&parent_thread_id)
                .map(|entry| entry.state.clone())
        };
        let Some(parent_state) = parent_state else {
            return;
        };
        let changed = {
            let mut parent_state = parent_state.lock().await;
            parent_state.listener_generation == parent_listener_generation
                && parent_state.live_spine_ui(&parent_turn_id).is_some()
                && parent_state.invalidate_spine_ui_agent_state(child_thread_id, generation)
        };
        if changed && let Some(tx) = self.current_listener_command_tx(parent_thread_id) {
            let _ = tx.send(ThreadListenerCommand::SpineUi(Box::new(
                Command::EmitSpineUiInvalidation {
                    parent_listener_generation,
                    turn_id: parent_turn_id,
                },
            )));
        }
    }
}
