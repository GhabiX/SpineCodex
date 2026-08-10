use super::*;
use crate::spine_ui::SpineUiState;
use codex_app_server_protocol::TurnStatus;
use codex_protocol::protocol::SpineSpawnProgressEvent;
use codex_protocol::protocol::SpineTreeUpdateEvent;

#[derive(Default)]
pub(super) struct SpineUiThreadRuntime {
    cumulative: SpineUiState,
    terminal: Option<SpineUiTerminal>,
    pending: Vec<SpineUiPendingTurn>,
    next_turn_order: u64,
    terminal_order: u64,
    next_revision: u64,
}

struct SpineUiTerminal {
    state: SpineUiState,
}

struct SpineUiPendingTurn {
    turn_id: String,
    state: SpineUiState,
    mounted_connection_ids: HashSet<ConnectionId>,
    order: u64,
}

pub(crate) struct SpineUiCompletion {
    pub(crate) state: SpineUiState,
    pub(crate) connection_ids: Vec<ConnectionId>,
}

impl SpineUiThreadRuntime {
    pub(super) fn clear_terminal(&mut self) {
        self.terminal = None;
    }

    pub(super) fn clear_transient_state(&mut self) {
        self.cumulative = SpineUiState::default();
        self.terminal = None;
        self.pending.clear();
    }
}

#[derive(Default)]
pub(super) struct SpineUiThreadState {
    active: SpineUiState,
    active_turn_id: Option<String>,
    mounted_connection_ids: HashSet<ConnectionId>,
    runtime: SpineUiThreadRuntime,
}

#[derive(Default)]
pub(super) struct SpineUiThreadEntryState {
    pub(super) listener_generation: u64,
    pub(super) active_turn_id: Option<String>,
    pub(super) terminal_ack: Option<SpineUiTerminalAck>,
}

#[derive(Default)]
pub(super) struct SpineUiManagerState {
    pub(super) parent_by_child: HashMap<ThreadId, SpineUiParentRoute>,
    pub(super) next_route_generation: u64,
}

impl SpineUiThreadState {
    pub(super) fn clear_transient_state(&mut self) {
        self.active = SpineUiState::default();
        self.active_turn_id = None;
        self.mounted_connection_ids.clear();
        self.runtime.clear_transient_state();
    }

    pub(super) fn start_turn(&mut self) {
        self.runtime.clear_terminal();
        self.active = SpineUiState::default();
        self.active_turn_id = None;
        self.mounted_connection_ids.clear();
    }

    pub(super) fn activate(&mut self, turn_id: &str) {
        if self.active_turn_id.as_deref() != Some(turn_id) {
            self.active = self.runtime.cumulative.carry_forward();
            self.active_turn_id = Some(turn_id.to_string());
            self.mounted_connection_ids.clear();
        }
    }

    pub(super) fn observe_event(&mut self, turn_id: &str, event: &EventMsg, has_active_turn: bool) {
        let activates = match event {
            EventMsg::RawResponseItem(payload) => {
                crate::spine_ui::is_tree_affecting_item(&payload.item)
            }
            EventMsg::SpineSpawnProgress(_) => true,
            _ => false,
        };
        if has_active_turn && activates {
            self.activate(turn_id);
        }
    }
}

#[derive(Clone)]
pub(super) struct SpineUiTerminalAck {
    pub(super) listener_generation: u64,
    pub(super) turn_id: String,
}

#[derive(Clone)]
pub(super) struct SpineUiParentRoute {
    pub(super) parent_thread_id: ThreadId,
    pub(super) parent_turn_id: String,
    expected_child_turn_id: Option<String>,
    parent_listener_generation: u64,
    baseline_node_ids: Arc<HashSet<String>>,
    generation: u64,
    queued_revision: Option<u64>,
    last_forwarded_revision: Option<u64>,
}

impl ThreadState {
    pub(crate) fn track_current_turn_event_for_listener(
        &mut self,
        listener_generation: u64,
        event_turn_id: &str,
        event: &EventMsg,
    ) -> Option<bool> {
        if self.listener_generation != listener_generation {
            return None;
        }
        self.track_current_turn_event(event_turn_id, event);
        Some(self.experimental_raw_events)
    }

    pub(crate) fn observe_spine_ui_event(&mut self, event_turn_id: &str, event: &EventMsg) {
        if let EventMsg::TurnStarted(_) = event {
            self.spine_ui.start_turn();
        }
        let has_active_turn = self.has_active_turn(event_turn_id);
        self.spine_ui
            .observe_event(event_turn_id, event, has_active_turn);
    }

    pub(crate) fn has_active_turn(&self, turn_id: &str) -> bool {
        self.current_turn_history
            .active_turn_snapshot()
            .is_some_and(|turn| turn.id == turn_id && turn.status == TurnStatus::InProgress)
    }

    pub(crate) fn live_spine_ui(&self, turn_id: &str) -> Option<&SpineUiState> {
        self.has_active_turn(turn_id)
            .then(|| {
                (self.spine_ui.active_turn_id.as_deref() == Some(turn_id))
                    .then_some(&self.spine_ui.active)
            })
            .flatten()
    }

    pub(crate) fn spine_ui_baseline_node_ids(&self, turn_id: &str) -> Option<HashSet<String>> {
        if !self.has_active_turn(turn_id)
            || self.spine_ui.active_turn_id.as_deref() != Some(turn_id)
        {
            return None;
        }
        let snapshot = self
            .spine_ui
            .active
            .latest_snapshot()
            .or_else(|| self.spine_ui.runtime.cumulative.latest_snapshot());
        Some(
            snapshot
                .map(|snapshot| {
                    snapshot
                        .nodes
                        .iter()
                        .map(|node| node.node_id.clone())
                        .collect()
                })
                .unwrap_or_default(),
        )
    }

    fn current_spine_ui_for_forward(&self) -> Option<&SpineUiState> {
        self.current_turn_history
            .active_turn_snapshot()
            .and_then(|turn| {
                (self.spine_ui.active_turn_id.as_deref() == Some(turn.id.as_str()))
                    .then_some(&self.spine_ui.active)
            })
            .filter(|state| state.latest_snapshot().is_some())
            .or_else(|| {
                self.spine_ui
                    .runtime
                    .pending
                    .iter()
                    .max_by_key(|pending| pending.order)
                    .map(|pending| &pending.state)
            })
            .or_else(|| {
                self.spine_ui
                    .runtime
                    .terminal
                    .as_ref()
                    .map(|terminal| &terminal.state)
            })
    }

    pub(crate) fn mark_spine_ui_mounted(&mut self, turn_id: &str, connection_ids: &[ConnectionId]) {
        if self.spine_ui.active_turn_id.as_deref() == Some(turn_id) {
            self.spine_ui
                .mounted_connection_ids
                .extend(connection_ids.iter().copied());
        }
    }

    pub(crate) fn mount_spine_ui_for_connection(
        &mut self,
        connection_id: ConnectionId,
    ) -> Option<(String, SpineUiState)> {
        if let Some(turn_id) = self.spine_ui.active_turn_id.clone()
            && self.spine_ui.active.latest_snapshot().is_some()
        {
            self.spine_ui.mounted_connection_ids.insert(connection_id);
            return Some((turn_id, self.spine_ui.active.clone()));
        }
        let pending = self
            .spine_ui
            .runtime
            .pending
            .iter_mut()
            .max_by_key(|pending| pending.order)?;
        pending.mounted_connection_ids.insert(connection_id);
        Some((pending.turn_id.clone(), pending.state.clone()))
    }

    pub(crate) fn active_spine_ui_snapshot(&self) -> Option<(String, SpineUiState)> {
        let turn = self.current_turn_history.active_turn_snapshot()?;
        (self.spine_ui.active_turn_id.as_deref() == Some(turn.id.as_str()))
            .then_some(&self.spine_ui.active)
            .filter(|state| state.latest_snapshot().is_some())
            .cloned()
            .map(|state| (turn.id, state))
    }

    pub(crate) fn record_spine_ui_snapshot(&mut self, snapshot: SpineTreeUpdateEvent) -> bool {
        if self.spine_ui.active_turn_id.is_none() || !self.current_turn_history.has_active_turn() {
            self.spine_ui.runtime.cumulative.record_snapshot(snapshot);
            return false;
        }
        let changed = self.spine_ui.active.record_snapshot(snapshot);
        if changed {
            self.assign_turn_summary_spine_ui_revision();
        }
        changed
    }

    pub(crate) fn record_spine_ui_spawn_progress(
        &mut self,
        progress: SpineSpawnProgressEvent,
    ) -> bool {
        if self.spine_ui.active_turn_id.is_none() || !self.current_turn_history.has_active_turn() {
            return false;
        }
        let changed = self.spine_ui.active.record_spawn_progress(progress);
        if changed {
            self.assign_turn_summary_spine_ui_revision();
        }
        changed
    }

    pub(crate) fn record_spine_ui_agent_state(
        &mut self,
        child_thread_id: ThreadId,
        generation: u64,
        child_state: SpineUiState,
    ) -> bool {
        let changed =
            self.spine_ui
                .active
                .record_agent_state(child_thread_id, generation, child_state);
        if changed {
            self.assign_turn_summary_spine_ui_revision();
        }
        changed
    }

    pub(crate) fn begin_spine_ui_turn_completion(&mut self, turn_id: &str) -> Option<u64> {
        if self.spine_ui.active_turn_id.as_deref() != Some(turn_id)
            || self.spine_ui.active.latest_snapshot().is_none()
            || self
                .spine_ui
                .runtime
                .pending
                .iter()
                .any(|pending| pending.turn_id == turn_id)
        {
            return None;
        }

        self.spine_ui.runtime.next_turn_order =
            self.spine_ui.runtime.next_turn_order.saturating_add(1);
        let state = std::mem::take(&mut self.spine_ui.active);
        self.spine_ui.runtime.cumulative = state.clone();
        let completion_token = self.spine_ui.runtime.next_turn_order;
        self.spine_ui.runtime.pending.push(SpineUiPendingTurn {
            turn_id: turn_id.to_string(),
            state,
            mounted_connection_ids: std::mem::take(&mut self.spine_ui.mounted_connection_ids),
            order: completion_token,
        });
        self.spine_ui.active_turn_id = None;
        Some(completion_token)
    }

    pub(crate) fn terminalize_spine_ui_incomplete_agents(
        &mut self,
        terminal_status: codex_protocol::protocol::AgentStatus,
    ) -> bool {
        let changed = self
            .spine_ui
            .active
            .terminalize_incomplete_agents(terminal_status);
        if changed {
            self.assign_turn_summary_spine_ui_revision();
        }
        changed
    }

    pub(crate) fn finalize_spine_ui_turn(
        &mut self,
        turn_id: &str,
        completion_token: u64,
    ) -> Option<SpineUiCompletion> {
        let pending_index =
            self.spine_ui.runtime.pending.iter().position(|pending| {
                pending.turn_id == turn_id && pending.order == completion_token
            })?;
        let mut pending = self.spine_ui.runtime.pending.remove(pending_index);
        pending.state.mark_completed();
        let revision = self.allocate_spine_ui_revision(pending.state.revision());
        pending.state.set_revision(revision);
        if pending.order >= self.spine_ui.runtime.terminal_order {
            self.spine_ui.runtime.terminal_order = pending.order;
            self.spine_ui.runtime.terminal = Some(SpineUiTerminal {
                state: pending.state.clone(),
            });
        }
        Some(SpineUiCompletion {
            state: pending.state,
            connection_ids: pending.mounted_connection_ids.into_iter().collect(),
        })
    }

    pub(crate) fn invalidate_spine_ui_agent_state(
        &mut self,
        child_thread_id: ThreadId,
        generation: u64,
    ) -> bool {
        let changed = self
            .spine_ui
            .active
            .remove_agent_state(child_thread_id, generation);
        if changed {
            self.assign_turn_summary_spine_ui_revision();
        }
        changed
    }

    fn assign_turn_summary_spine_ui_revision(&mut self) {
        let revision = self.allocate_spine_ui_revision(self.spine_ui.active.revision());
        self.spine_ui.active.set_revision(revision);
    }

    fn allocate_spine_ui_revision(&mut self, candidate: u64) -> u64 {
        let revision = self
            .spine_ui
            .runtime
            .next_revision
            .saturating_add(1)
            .max(candidate);
        self.spine_ui.runtime.next_revision = revision;
        revision
    }

    pub(crate) fn reset_spine_ui_after_rollback(&mut self) {
        self.clear_spine_ui_transient_state();
        self.last_terminal_turn_id = None;
    }

    pub(super) fn clear_spine_ui_transient_state(&mut self) {
        self.spine_ui.clear_transient_state();
    }

    #[cfg(test)]
    pub(crate) fn take_turn_summary(&mut self) -> TurnSummary {
        if let Some(turn_id) = self.spine_ui.active_turn_id.clone()
            && let Some(completion_token) = self.begin_spine_ui_turn_completion(&turn_id)
        {
            self.finalize_spine_ui_turn(&turn_id, completion_token);
        }
        std::mem::take(&mut self.turn_summary)
    }
}

#[path = "thread_state_spine_ui_manager.rs"]
mod manager;
pub(super) use manager::clear_routes;
pub(super) use manager::remove_thread_routes;

#[cfg(test)]
#[path = "thread_state_spine_ui_tests.rs"]
mod tests;
