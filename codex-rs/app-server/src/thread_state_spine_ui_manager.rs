use super::*;
use std::time::Duration;

impl ThreadStateManager {
    pub(crate) async fn note_spine_ui_listener_generation(
        &self,
        thread_id: ThreadId,
        listener_generation: u64,
    ) {
        let mut state = self.state.lock().await;
        let generation_changed = {
            let entry = state.threads.entry(thread_id).or_default();
            if listener_generation < entry.listener_generation {
                return;
            }
            let generation_changed = entry.listener_generation != listener_generation;
            if generation_changed {
                entry.spine_ui_terminal_ack = None;
            }
            entry.listener_generation = listener_generation;
            generation_changed
        };
        if generation_changed && let Some(route) = state.spine_ui_parent_by_child.get(&thread_id) {
            route
                .terminal_tx
                .send_replace(SpineUiRouteTerminalState::Pending);
        }
    }

    pub(crate) async fn note_spine_ui_agent_turn_started(
        &self,
        thread_id: ThreadId,
        listener_generation: u64,
    ) {
        let mut state = self.state.lock().await;
        let Some(entry) = state.threads.get(&thread_id) else {
            return;
        };
        if entry.listener_generation != listener_generation {
            return;
        }
        let stale_children = state
            .spine_ui_parent_by_child
            .iter()
            .filter_map(|(child_thread_id, route)| {
                (route.parent_thread_id == thread_id).then_some(*child_thread_id)
            })
            .collect::<Vec<_>>();
        for child_thread_id in stale_children {
            if let Some(route) = state.spine_ui_parent_by_child.remove(&child_thread_id) {
                route
                    .terminal_tx
                    .send_replace(SpineUiRouteTerminalState::Invalidated);
            }
        }

        let Some(entry) = state.threads.get_mut(&thread_id) else {
            return;
        };
        entry.spine_ui_terminal_ack = None;
        if let Some(route) = state.spine_ui_parent_by_child.get(&thread_id) {
            route
                .terminal_tx
                .send_replace(SpineUiRouteTerminalState::Pending);
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
        parent_turn_id: &str,
        progress: &SpineSpawnProgressEvent,
    ) {
        let parent_state = self.thread_state(parent_thread_id).await;
        let baseline_node_ids = Arc::new({
            let parent_state = parent_state.lock().await;
            let Some(parent_ui) = parent_state.live_spine_ui(parent_turn_id) else {
                return;
            };
            parent_ui
                .latest_snapshot()
                .map(|snapshot| {
                    snapshot
                        .nodes
                        .iter()
                        .map(|node| node.node_id.clone())
                        .collect()
                })
                .unwrap_or_default()
        });

        let new_child_thread_ids =
            {
                let mut state = self.state.lock().await;
                let mut new_child_thread_ids = Vec::new();
                for task in &progress.tasks {
                    let terminal_ack = {
                        let entry = state.threads.entry(task.thread_id).or_default();
                        entry
                            .spine_ui_terminal_ack
                            .clone()
                            .filter(|ack| entry.listener_generation == ack.listener_generation)
                    };
                    if state
                        .spine_ui_parent_by_child
                        .get(&task.thread_id)
                        .is_some_and(|route| {
                            route.parent_thread_id == parent_thread_id
                                && route.parent_turn_id == parent_turn_id
                        })
                    {
                        continue;
                    }
                    if let Some(previous) = state.spine_ui_parent_by_child.remove(&task.thread_id) {
                        previous
                            .terminal_tx
                            .send_replace(SpineUiRouteTerminalState::Invalidated);
                    }
                    state.next_spine_ui_route_generation =
                        state.next_spine_ui_route_generation.saturating_add(1);
                    let generation = state.next_spine_ui_route_generation;
                    let terminal_state =
                        terminal_ack.map_or(SpineUiRouteTerminalState::Pending, |ack| {
                            SpineUiRouteTerminalState::Settled(ack.state.map(|state| {
                                Box::new(state.filtered_for_parent(&baseline_node_ids))
                            }))
                        });
                    state.spine_ui_parent_by_child.insert(
                        task.thread_id,
                        SpineUiParentRoute {
                            parent_thread_id,
                            parent_turn_id: parent_turn_id.to_string(),
                            baseline_node_ids: baseline_node_ids.clone(),
                            generation,
                            timed_out: false,
                            queued_revision: None,
                            last_forwarded_revision: None,
                            terminal_tx: watch::channel(terminal_state).0,
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
        self.queue_spine_ui_agent_state_with_kind(child_thread_id, SpineUiAgentForwardKind::Live)
            .await;
    }

    pub(crate) async fn queue_spine_ui_agent_terminal_refresh(&self, child_thread_id: ThreadId) {
        self.queue_spine_ui_agent_state_with_kind(
            child_thread_id,
            SpineUiAgentForwardKind::TerminalRefresh,
        )
        .await;
    }

    async fn queue_spine_ui_agent_state_with_kind(
        &self,
        child_thread_id: ThreadId,
        kind: SpineUiAgentForwardKind,
    ) {
        let (route, child_state) = {
            let state = self.state.lock().await;
            let Some(route) = state
                .spine_ui_parent_by_child
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
        if kind == SpineUiAgentForwardKind::Live {
            let mut state = self.state.lock().await;
            let Some(current) = state.spine_ui_parent_by_child.get_mut(&child_thread_id) else {
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
        } else {
            let mut state = self.state.lock().await;
            let Some(current) = state.spine_ui_parent_by_child.get_mut(&child_thread_id) else {
                return;
            };
            if current.generation != route.generation {
                return;
            }
            current
                .terminal_tx
                .send_replace(SpineUiRouteTerminalState::Settled(Some(Box::new(
                    child_state.clone(),
                ))));
        }
        let send_result = tx.send(ThreadListenerCommand::ForwardSpineUiAgentState {
            child_thread_id,
            parent_turn_id: route.parent_turn_id,
            generation: route.generation,
            state: Some(child_state),
            terminal: kind == SpineUiAgentForwardKind::TerminalRefresh,
        });
        if send_result.is_err() && kind == SpineUiAgentForwardKind::Live {
            let mut state = self.state.lock().await;
            if let Some(current) = state.spine_ui_parent_by_child.get_mut(&child_thread_id)
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
            let Some(route) = state.spine_ui_parent_by_child.get_mut(&child_thread_id) else {
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
        let child_state = {
            let state = self.state.lock().await;
            let Some(entry) = state.threads.get(&child_thread_id) else {
                return;
            };
            if entry.listener_generation != listener_generation {
                return;
            }
            entry.state.clone()
        };
        let terminal_state = {
            let child_state = child_state.lock().await;
            if child_state.current_turn_history.has_active_turn()
                || child_state.listener_generation != listener_generation
                || child_state.last_terminal_turn_id.as_deref() != Some(terminal_turn_id)
            {
                return;
            }
            child_state.current_spine_ui_for_forward().cloned()
        };

        let mut state = self.state.lock().await;
        {
            let Some(entry) = state.threads.get_mut(&child_thread_id) else {
                return;
            };
            if entry.listener_generation != listener_generation {
                return;
            }
            entry.spine_ui_terminal_ack = Some(SpineUiTerminalAck {
                listener_generation,
                state: terminal_state.clone(),
            });
        }
        let route = state
            .spine_ui_parent_by_child
            .get(&child_thread_id)
            .cloned();
        let late_terminal = if let Some(route) = route
            && let Some(current) = state.spine_ui_parent_by_child.get_mut(&child_thread_id)
            && current.generation == route.generation
        {
            let was_timed_out = current.timed_out;
            let filtered_state = terminal_state
                .map(|state| Box::new(state.filtered_for_parent(&route.baseline_node_ids)));
            current
                .terminal_tx
                .send_replace(SpineUiRouteTerminalState::Settled(filtered_state.clone()));
            was_timed_out.then_some((route, filtered_state.map(|state| *state)))
        } else {
            None
        };
        drop(state);

        if let Some((route, terminal_state)) = late_terminal
            && let Some(tx) = self.current_listener_command_tx(route.parent_thread_id)
        {
            let _ = tx.send(ThreadListenerCommand::ForwardSpineUiAgentState {
                child_thread_id,
                parent_turn_id: route.parent_turn_id,
                generation: route.generation,
                state: terminal_state,
                terminal: true,
            });
        }
    }

    pub(crate) async fn wait_for_spine_ui_terminal_children(
        &self,
        parent_thread_id: ThreadId,
        parent_turn_id: &str,
        timeout: Duration,
    ) -> (
        Vec<(ThreadId, u64, Option<SpineUiState>)>,
        Vec<(ThreadId, u64)>,
    ) {
        let candidates = {
            let state = self.state.lock().await;
            state
                .spine_ui_parent_by_child
                .iter()
                .filter(|(_, route)| {
                    route.parent_thread_id == parent_thread_id
                        && route.parent_turn_id == parent_turn_id
                })
                .filter_map(|(child_thread_id, route)| {
                    let child_state = state.threads.get(child_thread_id)?.state.clone();
                    Some((
                        *child_thread_id,
                        route.generation,
                        route.baseline_node_ids.clone(),
                        route.terminal_tx.subscribe(),
                        child_state,
                    ))
                })
                .collect::<Vec<_>>()
        };

        let deadline = tokio::time::Instant::now() + timeout;
        let mut states = Vec::with_capacity(candidates.len());
        let mut timed_out = Vec::new();
        for (child_thread_id, generation, baseline_node_ids, mut terminal_rx, child_state) in
            candidates
        {
            let current = terminal_rx.borrow_and_update().clone();
            let terminal = if matches!(current, SpineUiRouteTerminalState::Pending) {
                tokio::time::timeout_at(deadline, async {
                    loop {
                        if terminal_rx.changed().await.is_err() {
                            break SpineUiRouteTerminalState::Invalidated;
                        }
                        let state = terminal_rx.borrow_and_update().clone();
                        if !matches!(state, SpineUiRouteTerminalState::Pending) {
                            break state;
                        }
                    }
                })
                .await
                .ok()
            } else {
                Some(current)
            };

            match terminal {
                Some(SpineUiRouteTerminalState::Settled(state)) => {
                    states.push((child_thread_id, generation, state.map(|state| *state)));
                }
                Some(SpineUiRouteTerminalState::Invalidated) => {
                    states.push((child_thread_id, generation, None));
                }
                Some(SpineUiRouteTerminalState::TimedOut) => {
                    timed_out.push((child_thread_id, generation));
                    states.push((
                        child_thread_id,
                        generation,
                        child_state
                            .lock()
                            .await
                            .current_spine_ui_for_forward()
                            .map(|state| state.filtered_for_parent(&baseline_node_ids)),
                    ));
                }
                Some(SpineUiRouteTerminalState::Pending) => unreachable!(),
                None => {
                    let latest = child_state
                        .lock()
                        .await
                        .current_spine_ui_for_forward()
                        .map(|state| state.filtered_for_parent(&baseline_node_ids));
                    match self
                        .mark_spine_ui_route_timed_out(
                            child_thread_id,
                            parent_thread_id,
                            parent_turn_id,
                            generation,
                        )
                        .await
                    {
                        SpineUiRouteTerminalState::Settled(state) => {
                            states.push((child_thread_id, generation, state.map(|state| *state)));
                        }
                        SpineUiRouteTerminalState::Invalidated => {
                            states.push((child_thread_id, generation, None));
                        }
                        SpineUiRouteTerminalState::TimedOut => {
                            timed_out.push((child_thread_id, generation));
                            states.push((child_thread_id, generation, latest));
                        }
                        SpineUiRouteTerminalState::Pending => unreachable!(),
                    }
                }
            }
        }
        (states, timed_out)
    }

    async fn mark_spine_ui_route_timed_out(
        &self,
        child_thread_id: ThreadId,
        parent_thread_id: ThreadId,
        parent_turn_id: &str,
        generation: u64,
    ) -> SpineUiRouteTerminalState {
        let mut state = self.state.lock().await;
        let Some(route) = state.spine_ui_parent_by_child.get_mut(&child_thread_id) else {
            return SpineUiRouteTerminalState::Invalidated;
        };
        if route.parent_thread_id != parent_thread_id
            || route.parent_turn_id != parent_turn_id
            || route.generation != generation
        {
            return SpineUiRouteTerminalState::Invalidated;
        }
        let terminal = route.terminal_tx.borrow().clone();
        if matches!(terminal, SpineUiRouteTerminalState::Pending) {
            route.timed_out = true;
            route
                .terminal_tx
                .send_replace(SpineUiRouteTerminalState::TimedOut);
            SpineUiRouteTerminalState::TimedOut
        } else {
            terminal
        }
    }

    pub(crate) async fn complete_spine_ui_late_terminal(
        &self,
        child_thread_id: ThreadId,
        parent_thread_id: ThreadId,
        parent_turn_id: &str,
        generation: u64,
    ) {
        let mut state = self.state.lock().await;
        let should_remove = state
            .spine_ui_parent_by_child
            .get(&child_thread_id)
            .is_some_and(|route| {
                route.parent_thread_id == parent_thread_id
                    && route.parent_turn_id == parent_turn_id
                    && route.generation == generation
                    && route.timed_out
            });
        if should_remove {
            state.spine_ui_parent_by_child.remove(&child_thread_id);
        }
    }

    pub(crate) async fn spine_ui_route_is_current(
        &self,
        child_thread_id: ThreadId,
        parent_thread_id: ThreadId,
        parent_turn_id: &str,
        generation: u64,
    ) -> bool {
        self.state
            .lock()
            .await
            .spine_ui_parent_by_child
            .get(&child_thread_id)
            .is_some_and(|route| {
                route.parent_thread_id == parent_thread_id
                    && route.parent_turn_id == parent_turn_id
                    && route.generation == generation
            })
    }

    pub(crate) async fn clear_spine_ui_parent_routes(
        &self,
        parent_thread_id: ThreadId,
        parent_turn_id: &str,
    ) {
        let mut state = self.state.lock().await;
        let child_thread_ids = state
            .spine_ui_parent_by_child
            .iter()
            .filter_map(|(child_thread_id, route)| {
                (route.parent_thread_id == parent_thread_id
                    && route.parent_turn_id == parent_turn_id
                    && !route.timed_out
                    && !route.has_nested_sync_timeout())
                .then_some(*child_thread_id)
            })
            .collect::<Vec<_>>();
        for child_thread_id in child_thread_ids {
            if let Some(route) = state.spine_ui_parent_by_child.remove(&child_thread_id) {
                route
                    .terminal_tx
                    .send_replace(SpineUiRouteTerminalState::Invalidated);
            }
        }
    }

    pub(crate) async fn clear_all_spine_ui_routes_for_thread(&self, thread_id: ThreadId) {
        self.clear_spine_ui_routes_for_thread(thread_id, None).await;
    }

    pub(crate) async fn clear_spine_ui_routes_for_listener_exit(
        &self,
        thread_id: ThreadId,
        listener_generation: u64,
    ) {
        self.clear_spine_ui_routes_for_thread(thread_id, Some(listener_generation))
            .await;
    }

    async fn clear_spine_ui_routes_for_thread(
        &self,
        thread_id: ThreadId,
        listener_generation: Option<u64>,
    ) {
        let invalidation_targets = {
            let mut state = self.state.lock().await;
            if listener_generation.is_some_and(|listener_generation| {
                state
                    .threads
                    .get(&thread_id)
                    .is_none_or(|entry| entry.listener_generation != listener_generation)
            }) {
                return;
            }
            if let Some(entry) = state.threads.get_mut(&thread_id) {
                entry.spine_ui_terminal_ack = None;
            }
            let routes = state
                .spine_ui_parent_by_child
                .iter()
                .filter_map(|(child_thread_id, route)| {
                    (*child_thread_id == thread_id || route.parent_thread_id == thread_id)
                        .then_some((*child_thread_id, route.clone()))
                })
                .collect::<Vec<_>>();
            let mut targets = Vec::new();
            for (child_thread_id, route) in routes {
                state.spine_ui_parent_by_child.remove(&child_thread_id);
                route
                    .terminal_tx
                    .send_replace(SpineUiRouteTerminalState::Invalidated);
                if child_thread_id == thread_id {
                    targets.push((route.parent_thread_id, child_thread_id, route.generation));
                }
            }
            targets
        };

        for (parent_thread_id, child_thread_id, generation) in invalidation_targets {
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
            let active = {
                let mut parent_state = parent_state.lock().await;
                parent_state
                    .invalidate_spine_ui_agent_state(child_thread_id, generation)
                    .then(|| parent_state.active_spine_ui_snapshot())
                    .flatten()
            };
            if let Some((turn_id, state)) = active
                && let Some(tx) = self.current_listener_command_tx(parent_thread_id)
            {
                let _ = tx.send(ThreadListenerCommand::EmitSpineUiInvalidation { turn_id, state });
            }
        }
    }
}
