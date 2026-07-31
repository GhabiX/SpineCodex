use super::*;
use codex_app_server_protocol::TurnStatus;

#[derive(Default)]
pub(super) struct SpineUiThreadRuntime {
    cumulative: SpineUiState,
    terminal: Option<SpineUiTerminal>,
    next_revision: u64,
}

struct SpineUiTerminal {
    turn_id: String,
    state: SpineUiState,
    connection_ids: Vec<ConnectionId>,
}

impl SpineUiThreadRuntime {
    pub(super) fn clear_terminal(&mut self) {
        self.terminal = None;
    }

    pub(super) fn clear_transient_state(&mut self) {
        self.cumulative = SpineUiState::default();
        self.terminal = None;
    }
}

#[derive(Clone)]
pub(super) struct SpineUiTerminalAck {
    pub(super) listener_generation: u64,
    pub(super) state: Option<SpineUiState>,
}

#[derive(Clone)]
pub(super) struct SpineUiParentRoute {
    pub(super) parent_thread_id: ThreadId,
    pub(super) parent_turn_id: String,
    baseline_node_ids: Arc<HashSet<String>>,
    generation: u64,
    timed_out: bool,
    queued_revision: Option<u64>,
    last_forwarded_revision: Option<u64>,
    pub(super) terminal_tx: watch::Sender<SpineUiRouteTerminalState>,
}

impl SpineUiParentRoute {
    fn has_nested_sync_timeout(&self) -> bool {
        match &*self.terminal_tx.borrow() {
            SpineUiRouteTerminalState::Settled(Some(state)) => state.has_agent_sync_timeout(),
            SpineUiRouteTerminalState::Pending
            | SpineUiRouteTerminalState::TimedOut
            | SpineUiRouteTerminalState::Settled(None)
            | SpineUiRouteTerminalState::Invalidated => false,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(super) enum SpineUiRouteTerminalState {
    #[default]
    Pending,
    TimedOut,
    Settled(Option<Box<SpineUiState>>),
    Invalidated,
}

pub(crate) struct SpineUiAgentRefresh {
    pub(crate) state: SpineUiState,
    pub(crate) connection_ids: Vec<ConnectionId>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SpineUiAgentForwardKind {
    Live,
    TerminalRefresh,
}

impl ThreadState {
    pub(crate) fn has_active_turn(&self, turn_id: &str) -> bool {
        self.current_turn_history
            .active_turn_snapshot()
            .is_some_and(|turn| turn.id == turn_id && turn.status == TurnStatus::InProgress)
    }

    pub(crate) fn live_spine_ui(&self, turn_id: &str) -> Option<&SpineUiState> {
        self.has_active_turn(turn_id)
            .then(|| self.turn_summary.active_spine_ui(turn_id))
            .flatten()
    }

    fn current_spine_ui_for_forward(&self) -> Option<&SpineUiState> {
        self.current_turn_history
            .active_turn_snapshot()
            .and_then(|turn| self.turn_summary.active_spine_ui(&turn.id))
            .filter(|state| state.latest_snapshot().is_some())
            .or_else(|| {
                self.spine_ui_runtime
                    .terminal
                    .as_ref()
                    .map(|terminal| &terminal.state)
            })
    }

    pub(crate) fn active_spine_ui_snapshot(&self) -> Option<(String, SpineUiState)> {
        let turn = self.current_turn_history.active_turn_snapshot()?;
        self.turn_summary
            .active_spine_ui(&turn.id)
            .filter(|state| state.latest_snapshot().is_some())
            .cloned()
            .map(|state| (turn.id, state))
    }

    pub(crate) fn record_spine_ui_snapshot(&mut self, snapshot: SpineTreeUpdateEvent) -> bool {
        if self.turn_summary.spine_ui_turn_id.is_none()
            || !self.current_turn_history.has_active_turn()
        {
            self.spine_ui_runtime.cumulative.record_snapshot(snapshot);
            return false;
        }
        let changed = self.turn_summary.spine_ui.record_snapshot(snapshot);
        if changed {
            self.assign_turn_summary_spine_ui_revision();
        }
        changed
    }

    pub(crate) fn record_spine_ui_spawn_progress(
        &mut self,
        progress: SpineSpawnProgressEvent,
    ) -> bool {
        if self.turn_summary.spine_ui_turn_id.is_none()
            || !self.current_turn_history.has_active_turn()
        {
            return false;
        }
        let changed = self.turn_summary.spine_ui.record_spawn_progress(progress);
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
            self.turn_summary
                .spine_ui
                .record_agent_state(child_thread_id, generation, child_state);
        if changed {
            self.assign_turn_summary_spine_ui_revision();
        }
        changed
    }

    pub(crate) fn mark_spine_ui_agent_sync_timeout(
        &mut self,
        child_thread_id: ThreadId,
        generation: u64,
    ) -> bool {
        let changed = self
            .turn_summary
            .spine_ui
            .mark_agent_sync_timeout(child_thread_id, generation);
        if changed {
            self.assign_turn_summary_spine_ui_revision();
        }
        changed
    }

    pub(crate) fn record_completed_spine_ui_agent_terminal(
        &mut self,
        parent_turn_id: &str,
        child_thread_id: ThreadId,
        generation: u64,
        child_state: Option<SpineUiState>,
    ) -> Option<SpineUiAgentRefresh> {
        let mut terminal = self.spine_ui_runtime.terminal.take()?;
        if terminal.turn_id != parent_turn_id {
            self.spine_ui_runtime.terminal = Some(terminal);
            return None;
        }

        let changed = match child_state {
            Some(child_state) => {
                let state_changed =
                    terminal
                        .state
                        .record_agent_state(child_thread_id, generation, child_state);
                let timeout_cleared = terminal
                    .state
                    .clear_agent_sync_timeout(child_thread_id, generation);
                state_changed || timeout_cleared
            }
            None => terminal
                .state
                .remove_agent_state(child_thread_id, generation),
        };
        if !changed {
            self.spine_ui_runtime.terminal = Some(terminal);
            return None;
        }

        let revision = self.allocate_spine_ui_revision(terminal.state.revision());
        terminal.state.set_revision(revision);
        self.spine_ui_runtime.cumulative = terminal.state.clone();
        let refresh = SpineUiAgentRefresh {
            state: terminal.state.clone(),
            connection_ids: terminal.connection_ids.clone(),
        };
        self.spine_ui_runtime.terminal = Some(terminal);
        Some(refresh)
    }

    pub(crate) fn set_spine_ui_terminal_connection_ids(
        &mut self,
        turn_id: &str,
        connection_ids: &[ConnectionId],
    ) {
        if let Some(terminal) = self.spine_ui_runtime.terminal.as_mut()
            && terminal.turn_id == turn_id
        {
            terminal.connection_ids = connection_ids.to_vec();
        }
    }

    pub(crate) fn remove_spine_ui_terminal_connection_id(&mut self, connection_id: ConnectionId) {
        if let Some(terminal) = self.spine_ui_runtime.terminal.as_mut() {
            terminal
                .connection_ids
                .retain(|candidate| *candidate != connection_id);
        }
    }

    pub(crate) fn invalidate_spine_ui_agent_state(
        &mut self,
        child_thread_id: ThreadId,
        generation: u64,
    ) -> bool {
        let changed = self
            .turn_summary
            .spine_ui
            .remove_agent_state(child_thread_id, generation);
        if changed {
            self.assign_turn_summary_spine_ui_revision();
        }
        changed
    }

    fn assign_turn_summary_spine_ui_revision(&mut self) {
        let revision = self.allocate_spine_ui_revision(self.turn_summary.spine_ui.revision());
        self.turn_summary.spine_ui.set_revision(revision);
    }

    fn allocate_spine_ui_revision(&mut self, candidate: u64) -> u64 {
        let revision = self
            .spine_ui_runtime
            .next_revision
            .saturating_add(1)
            .max(candidate);
        self.spine_ui_runtime.next_revision = revision;
        revision
    }

    pub(crate) fn reset_spine_ui_after_rollback(&mut self) {
        self.clear_spine_ui_transient_state();
        self.last_terminal_turn_id = None;
    }

    pub(super) fn clear_spine_ui_transient_state(&mut self) {
        self.turn_summary.spine_ui = SpineUiState::default();
        self.turn_summary.spine_ui_turn_id = None;
        self.spine_ui_runtime.clear_transient_state();
    }

    pub(crate) fn take_turn_summary(&mut self) -> TurnSummary {
        let mut turn_summary = std::mem::take(&mut self.turn_summary);
        if let Some(turn_id) = turn_summary.spine_ui_turn_id.as_ref()
            && turn_summary.spine_ui.latest_snapshot().is_some()
        {
            turn_summary.spine_ui.mark_completed();
            self.spine_ui_runtime.cumulative = turn_summary.spine_ui.clone();
            self.spine_ui_runtime.terminal = Some(SpineUiTerminal {
                turn_id: turn_id.clone(),
                state: turn_summary.spine_ui.clone(),
                connection_ids: Vec::new(),
            });
        }
        turn_summary
    }

    pub(super) fn start_spine_ui_turn(&mut self) {
        self.spine_ui_runtime.clear_terminal();
        self.turn_summary.spine_ui = SpineUiState::default();
        self.turn_summary.spine_ui_turn_id = None;
    }

    pub(super) fn activate_spine_ui(&mut self, turn_id: &str) {
        if self.turn_summary.spine_ui_turn_id.as_deref() != Some(turn_id) {
            self.turn_summary.spine_ui = self.spine_ui_runtime.cumulative.carry_forward();
            self.turn_summary.spine_ui_turn_id = Some(turn_id.to_string());
        }
    }
}

#[path = "thread_state_spine_ui_manager.rs"]
mod manager;

#[cfg(test)]
#[path = "thread_state_spine_ui_tests.rs"]
mod tests;
