use super::SpineUiState;
use super::SpineUiTerminalOutcome;
use super::is_enabled;
use super::snapshot_terminal_notification;
use super::snapshot_upsert_notification;
use crate::outgoing_message::ConnectionId;
use crate::outgoing_message::OutgoingMessageSender;
use crate::outgoing_message::ThreadScopedOutgoingMessageSender;
use crate::thread_state::ThreadListenerCommand;
use crate::thread_state::ThreadState;
use crate::thread_state::ThreadStateManager;
use codex_app_server_protocol::ServerNotification;
use codex_protocol::ThreadId;
use codex_protocol::protocol::AgentStatus;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::TurnAbortReason;
use std::sync::Arc;
use tokio::sync::Mutex;

// `ThreadListenerCommand` boxes this entire command before it crosses the listener boundary.
#[allow(clippy::large_enum_variant)]
pub(crate) enum Command {
    ForwardSpineUiAgentState {
        child_thread_id: ThreadId,
        parent_turn_id: String,
        parent_listener_generation: u64,
        generation: u64,
        state: Option<SpineUiState>,
    },
    EmitSpineUiInvalidation {
        parent_listener_generation: u64,
        turn_id: String,
    },
}

pub(crate) async fn listener_started(
    manager: &ThreadStateManager,
    thread_id: ThreadId,
    generation: u64,
) {
    if !is_enabled() {
        return;
    }
    manager
        .note_spine_ui_listener_generation(thread_id, generation)
        .await;
}

pub(crate) async fn mount_for_connection(
    outgoing: &Arc<OutgoingMessageSender>,
    thread_state: &Arc<Mutex<ThreadState>>,
    thread_id: ThreadId,
    connection_id: ConnectionId,
) {
    if !is_enabled() {
        return;
    }
    let notification = {
        let mut state = thread_state.lock().await;
        state
            .mount_spine_ui_for_connection(connection_id)
            .and_then(|(turn_id, spine_ui)| {
                snapshot_upsert_notification(&thread_id.to_string(), &turn_id, &spine_ui)
            })
    };
    if let Some(notification) = notification {
        ThreadScopedOutgoingMessageSender::new(outgoing.clone(), vec![connection_id], thread_id)
            .send_server_notification(ServerNotification::ItemStarted(notification))
            .await;
    }
}

pub(crate) async fn after_track(
    manager: &ThreadStateManager,
    thread_state: &Arc<Mutex<ThreadState>>,
    thread_id: ThreadId,
    listener_generation: u64,
    event: &Event,
) {
    if !is_enabled() {
        return;
    }
    after_track_enabled(manager, thread_state, thread_id, listener_generation, event).await;
}

#[expect(
    clippy::await_holding_invalid_type,
    reason = "rollback route cleanup must be serialized against listener replacement"
)]
async fn after_track_enabled(
    manager: &ThreadStateManager,
    thread_state: &Arc<Mutex<ThreadState>>,
    thread_id: ThreadId,
    listener_generation: u64,
    event: &Event,
) {
    let rolled_back = matches!(&event.msg, EventMsg::ThreadRolledBack(_));
    let is_current = {
        let mut state = thread_state.lock().await;
        if state.listener_generation != listener_generation {
            false
        } else {
            state.observe_spine_ui_event(&event.id, &event.msg);
            if rolled_back {
                state.reset_spine_ui_after_rollback();
                // Keep the authoritative generation stable until route cleanup
                // finishes, just like listener-exit cleanup below.
                manager
                    .clear_spine_ui_routes_for_listener_exit(
                        thread_id,
                        listener_generation,
                        thread_state,
                    )
                    .await;
            }
            true
        }
    };
    if !is_current {
        return;
    }
    if rolled_back {
        return;
    }
    if matches!(&event.msg, EventMsg::TurnStarted(_)) {
        manager
            .note_spine_ui_agent_turn_started(thread_id, listener_generation, &event.id)
            .await;
    }
    if let EventMsg::SpineSpawnProgress(progress) = &event.msg {
        manager
            .register_spine_ui_spawn_progress(thread_id, listener_generation, &event.id, progress)
            .await;
    }
}

#[cfg(test)]
pub(crate) async fn after_track_enabled_for_test(
    manager: &ThreadStateManager,
    thread_state: &Arc<Mutex<ThreadState>>,
    thread_id: ThreadId,
    listener_generation: u64,
    event: &Event,
) {
    after_track_enabled(manager, thread_state, thread_id, listener_generation, event).await;
}

pub(crate) async fn before_bespoke(
    outgoing: &Arc<OutgoingMessageSender>,
    connection_ids: &[ConnectionId],
    thread_state: &Arc<Mutex<ThreadState>>,
    thread_id: ThreadId,
    listener_generation: u64,
    event: &Event,
) -> Option<u64> {
    if !is_enabled() {
        return None;
    }
    before_bespoke_enabled(
        outgoing,
        connection_ids,
        thread_state,
        thread_id,
        listener_generation,
        event,
    )
    .await
}

async fn before_bespoke_enabled(
    outgoing: &Arc<OutgoingMessageSender>,
    connection_ids: &[ConnectionId],
    thread_state: &Arc<Mutex<ThreadState>>,
    thread_id: ThreadId,
    listener_generation: u64,
    event: &Event,
) -> Option<u64> {
    let (notification, completion_token) = {
        let mut state = thread_state.lock().await;
        if state.listener_generation != listener_generation {
            return None;
        }
        match &event.msg {
            EventMsg::SpineTreeUpdate(snapshot) => {
                let notification = state
                    .record_spine_ui_snapshot(snapshot.clone())
                    .then(|| state.live_spine_ui(&event.id).cloned())
                    .flatten()
                    .and_then(|state| {
                        snapshot_upsert_notification(&thread_id.to_string(), &event.id, &state)
                    })
                    .map(ServerNotification::ItemStarted);
                if notification.is_some() {
                    state.mark_spine_ui_mounted(&event.id, connection_ids);
                }
                (notification, None)
            }
            EventMsg::SpineSpawnProgress(progress) => {
                let notification = state
                    .record_spine_ui_spawn_progress(progress.clone())
                    .then(|| state.live_spine_ui(&event.id).cloned())
                    .flatten()
                    .and_then(|state| {
                        snapshot_upsert_notification(&thread_id.to_string(), &event.id, &state)
                    })
                    .map(ServerNotification::ItemStarted);
                if notification.is_some() {
                    state.mark_spine_ui_mounted(&event.id, connection_ids);
                }
                (notification, None)
            }
            EventMsg::TurnComplete(_) => (None, state.begin_spine_ui_turn_completion(&event.id)),
            EventMsg::TurnAborted(abort_event) => {
                let terminal_status = match &abort_event.reason {
                    TurnAbortReason::Interrupted | TurnAbortReason::BudgetLimited => {
                        AgentStatus::Interrupted
                    }
                    TurnAbortReason::Replaced | TurnAbortReason::ReviewEnded => {
                        AgentStatus::Errored(format!(
                            "child turn aborted: {:?}",
                            abort_event.reason
                        ))
                    }
                };
                state.terminalize_spine_ui_incomplete_agents(terminal_status);
                (None, state.begin_spine_ui_turn_completion(&event.id))
            }
            _ => (None, None),
        }
    };
    if let Some(notification) = notification {
        ThreadScopedOutgoingMessageSender::new(
            outgoing.clone(),
            connection_ids.to_vec(),
            thread_id,
        )
        .send_server_notification(notification)
        .await;
    }
    completion_token
}

#[cfg(test)]
pub(crate) async fn before_bespoke_enabled_for_test(
    outgoing: &Arc<OutgoingMessageSender>,
    connection_ids: &[ConnectionId],
    thread_state: &Arc<Mutex<ThreadState>>,
    thread_id: ThreadId,
    listener_generation: u64,
    event: &Event,
) -> Option<u64> {
    before_bespoke_enabled(
        outgoing,
        connection_ids,
        thread_state,
        thread_id,
        listener_generation,
        event,
    )
    .await
}

pub(crate) async fn after_event(
    manager: &ThreadStateManager,
    thread_state: &Arc<Mutex<ThreadState>>,
    outgoing: &Arc<OutgoingMessageSender>,
    thread_id: ThreadId,
    listener_generation: u64,
    event: &Event,
    completion_token: Option<u64>,
) {
    if !is_enabled() {
        return;
    }
    after_event_enabled(
        manager,
        thread_state,
        outgoing,
        thread_id,
        listener_generation,
        event,
        completion_token,
    )
    .await;
}

async fn after_event_enabled(
    manager: &ThreadStateManager,
    thread_state: &Arc<Mutex<ThreadState>>,
    outgoing: &Arc<OutgoingMessageSender>,
    thread_id: ThreadId,
    listener_generation: u64,
    event: &Event,
    completion_token: Option<u64>,
) {
    let terminal_outcome = match &event.msg {
        EventMsg::TurnComplete(_) => Some(SpineUiTerminalOutcome::Completed),
        EventMsg::TurnAborted(event) => Some(SpineUiTerminalOutcome::Aborted(event.reason.clone())),
        _ => None,
    };
    if let Some(terminal_outcome) = terminal_outcome {
        let turn_id = event.id.clone();
        let Some(completion_token) = completion_token else {
            manager
                .acknowledge_spine_ui_agent_terminal(thread_id, listener_generation, &turn_id)
                .await;
            manager
                .clear_spine_ui_parent_routes(thread_id, &turn_id, listener_generation)
                .await;
            return;
        };
        finalize_turn(
            thread_id,
            manager,
            thread_state,
            outgoing,
            listener_generation,
            turn_id,
            completion_token,
            terminal_outcome,
        )
        .await;
        return;
    }
    if matches!(
        &event.msg,
        EventMsg::TurnStarted(_) | EventMsg::SpineTreeUpdate(_) | EventMsg::SpineSpawnProgress(_)
    ) {
        manager.queue_spine_ui_agent_state(thread_id).await;
    }
}

#[cfg(test)]
pub(crate) async fn after_event_enabled_for_test(
    manager: &ThreadStateManager,
    thread_state: &Arc<Mutex<ThreadState>>,
    outgoing: &Arc<OutgoingMessageSender>,
    thread_id: ThreadId,
    listener_generation: u64,
    event: &Event,
    completion_token: Option<u64>,
) {
    after_event_enabled(
        manager,
        thread_state,
        outgoing,
        thread_id,
        listener_generation,
        event,
        completion_token,
    )
    .await;
}

pub(crate) async fn listener_exited(
    manager: &ThreadStateManager,
    thread_state: &Arc<Mutex<ThreadState>>,
    thread_id: ThreadId,
    listener_generation: u64,
) {
    if !is_enabled() {
        return;
    }
    listener_exited_enabled(manager, thread_state, thread_id, listener_generation).await;
}

#[expect(
    clippy::await_holding_invalid_type,
    reason = "listener exit route cleanup must be serialized against listener replacement"
)]
async fn listener_exited_enabled(
    manager: &ThreadStateManager,
    thread_state: &Arc<Mutex<ThreadState>>,
    thread_id: ThreadId,
    listener_generation: u64,
) {
    // Keep the authoritative listener generation stable until route cleanup
    // finishes. `set_listener` uses the same lock, so a replacement cannot
    // publish a new generation between this check and the deletion.
    let state = thread_state.lock().await;
    if state.listener_generation != listener_generation {
        return;
    }
    manager
        .clear_spine_ui_routes_for_listener_exit(thread_id, listener_generation, thread_state)
        .await;
}

#[cfg(test)]
pub(crate) async fn listener_exited_enabled_for_test(
    manager: &ThreadStateManager,
    thread_state: &Arc<Mutex<ThreadState>>,
    thread_id: ThreadId,
    listener_generation: u64,
) {
    listener_exited_enabled(manager, thread_state, thread_id, listener_generation).await;
}

pub(crate) async fn listener_failed(
    manager: &ThreadStateManager,
    thread_state: &Arc<Mutex<ThreadState>>,
    outgoing: &Arc<OutgoingMessageSender>,
    thread_id: ThreadId,
    listener_generation: u64,
    error: String,
) {
    if !is_enabled() {
        return;
    }
    listener_failed_enabled(
        manager,
        thread_state,
        outgoing,
        thread_id,
        listener_generation,
        error,
    )
    .await;
}

async fn listener_failed_enabled(
    manager: &ThreadStateManager,
    thread_state: &Arc<Mutex<ThreadState>>,
    outgoing: &Arc<OutgoingMessageSender>,
    thread_id: ThreadId,
    listener_generation: u64,
    error: String,
) -> bool {
    let terminal = {
        let mut state = thread_state.lock().await;
        if state.listener_generation != listener_generation {
            return false;
        }
        let Some(turn_id) = state.active_turn_snapshot().map(|turn| turn.id) else {
            return false;
        };
        state.terminalize_spine_ui_incomplete_agents(AgentStatus::Errored(error.clone()));
        let Some(completion_token) = state.begin_spine_ui_turn_completion(&turn_id) else {
            return false;
        };
        state
            .finalize_spine_ui_turn(&turn_id, completion_token)
            .map(|completion| (turn_id, completion))
    };
    let Some((turn_id, completion)) = terminal else {
        return false;
    };
    send_terminal_notification(
        manager,
        outgoing,
        thread_id,
        &turn_id,
        &completion.state,
        completion.connection_ids,
        &SpineUiTerminalOutcome::ListenerFailed(error),
    )
    .await;
    true
}

#[cfg(test)]
pub(crate) async fn listener_failed_enabled_for_test(
    manager: &ThreadStateManager,
    thread_state: &Arc<Mutex<ThreadState>>,
    outgoing: &Arc<OutgoingMessageSender>,
    thread_id: ThreadId,
    listener_generation: u64,
    error: String,
) -> bool {
    listener_failed_enabled(
        manager,
        thread_state,
        outgoing,
        thread_id,
        listener_generation,
        error,
    )
    .await
}

pub(crate) async fn handle_command(
    thread_id: ThreadId,
    manager: &ThreadStateManager,
    thread_state: &Arc<Mutex<ThreadState>>,
    outgoing: &Arc<OutgoingMessageSender>,
    command: ThreadListenerCommand,
) -> Option<ThreadListenerCommand> {
    let ThreadListenerCommand::SpineUi(command) = command else {
        return Some(command);
    };
    if !is_enabled() {
        return None;
    }
    match *command {
        Command::ForwardSpineUiAgentState {
            child_thread_id,
            parent_turn_id,
            parent_listener_generation,
            generation,
            state: child_state,
        } => {
            forward_agent_state(
                thread_id,
                manager,
                thread_state,
                outgoing,
                child_thread_id,
                parent_turn_id,
                parent_listener_generation,
                generation,
                child_state,
            )
            .await;
        }
        Command::EmitSpineUiInvalidation {
            parent_listener_generation,
            turn_id,
        } => {
            emit_invalidation(
                thread_id,
                manager,
                thread_state,
                outgoing,
                parent_listener_generation,
                turn_id,
            )
            .await;
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
async fn forward_agent_state(
    thread_id: ThreadId,
    manager: &ThreadStateManager,
    thread_state: &Arc<Mutex<ThreadState>>,
    outgoing: &Arc<OutgoingMessageSender>,
    child_thread_id: ThreadId,
    parent_turn_id: String,
    parent_listener_generation: u64,
    generation: u64,
    child_state: Option<SpineUiState>,
) {
    if !manager
        .spine_ui_route_is_current(
            child_thread_id,
            thread_id,
            &parent_turn_id,
            parent_listener_generation,
            generation,
        )
        .await
    {
        return;
    }
    let forwarded_revision = child_state.as_ref().map(SpineUiState::revision);
    let connection_ids = manager.subscribed_connection_ids(thread_id).await;
    let spine_ui = {
        let mut state = thread_state.lock().await;
        let changed = state.listener_generation == parent_listener_generation
            && state.live_spine_ui(&parent_turn_id).is_some()
            && child_state.is_some_and(|child_state| {
                state.record_spine_ui_agent_state(child_thread_id, generation, child_state)
            });
        let spine_ui = changed
            .then(|| state.live_spine_ui(&parent_turn_id).cloned())
            .flatten();
        if spine_ui.is_some() {
            state.mark_spine_ui_mounted(&parent_turn_id, &connection_ids);
        }
        spine_ui
    };
    if let Some(spine_ui) = spine_ui {
        if let Some(notification) =
            snapshot_upsert_notification(&thread_id.to_string(), &parent_turn_id, &spine_ui)
        {
            ThreadScopedOutgoingMessageSender::new(outgoing.clone(), connection_ids, thread_id)
                .send_server_notification(ServerNotification::ItemStarted(notification))
                .await;
        }
        manager.queue_spine_ui_agent_state(thread_id).await;
    }
    if let Some(forwarded_revision) = forwarded_revision {
        manager
            .complete_spine_ui_agent_state_forward(
                child_thread_id,
                thread_id,
                &parent_turn_id,
                generation,
                forwarded_revision,
            )
            .await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn finalize_turn(
    thread_id: ThreadId,
    manager: &ThreadStateManager,
    thread_state: &Arc<Mutex<ThreadState>>,
    outgoing: &Arc<OutgoingMessageSender>,
    listener_generation: u64,
    turn_id: String,
    completion_token: u64,
    terminal_outcome: SpineUiTerminalOutcome,
) {
    let completion = {
        let mut state = thread_state.lock().await;
        if state.listener_generation != listener_generation {
            return;
        }
        state.finalize_spine_ui_turn(&turn_id, completion_token)
    };
    let Some(completion) = completion else {
        return;
    };
    send_terminal_notification(
        manager,
        outgoing,
        thread_id,
        &turn_id,
        &completion.state,
        completion.connection_ids,
        &terminal_outcome,
    )
    .await;
    manager
        .acknowledge_spine_ui_agent_terminal(thread_id, listener_generation, &turn_id)
        .await;
    // Once completed, a later child acknowledgement is only available to future routes.
    manager
        .clear_spine_ui_parent_routes(thread_id, &turn_id, listener_generation)
        .await;
}

async fn send_terminal_notification(
    manager: &ThreadStateManager,
    outgoing: &Arc<OutgoingMessageSender>,
    thread_id: ThreadId,
    turn_id: &str,
    state: &SpineUiState,
    connection_ids: Vec<ConnectionId>,
    terminal_outcome: &SpineUiTerminalOutcome,
) {
    let Some(notification) =
        snapshot_terminal_notification(&thread_id.to_string(), turn_id, state, terminal_outcome)
    else {
        return;
    };
    let subscribed = manager.subscribed_connection_ids(thread_id).await;
    let connection_ids = connection_ids
        .into_iter()
        .filter(|connection_id| subscribed.contains(connection_id))
        .collect();
    ThreadScopedOutgoingMessageSender::new(outgoing.clone(), connection_ids, thread_id)
        .send_server_notification(ServerNotification::ItemCompleted(notification))
        .await;
}

async fn emit_invalidation(
    thread_id: ThreadId,
    manager: &ThreadStateManager,
    thread_state: &Arc<Mutex<ThreadState>>,
    outgoing: &Arc<OutgoingMessageSender>,
    parent_listener_generation: u64,
    turn_id: String,
) -> bool {
    let connection_ids = manager.subscribed_connection_ids(thread_id).await;
    let state = {
        let mut state = thread_state.lock().await;
        if state.listener_generation != parent_listener_generation {
            return false;
        }
        let spine_ui = state.live_spine_ui(&turn_id).cloned();
        if spine_ui.is_some() {
            state.mark_spine_ui_mounted(&turn_id, &connection_ids);
        }
        spine_ui
    };
    let Some(state) = state else {
        return false;
    };
    let Some(notification) = snapshot_upsert_notification(&thread_id.to_string(), &turn_id, &state)
    else {
        return false;
    };
    ThreadScopedOutgoingMessageSender::new(outgoing.clone(), connection_ids, thread_id)
        .send_server_notification(ServerNotification::ItemStarted(notification))
        .await;
    true
}

#[cfg(test)]
pub(crate) async fn emit_invalidation_enabled_for_test(
    thread_id: ThreadId,
    manager: &ThreadStateManager,
    thread_state: &Arc<Mutex<ThreadState>>,
    outgoing: &Arc<OutgoingMessageSender>,
    parent_listener_generation: u64,
    turn_id: String,
) -> bool {
    emit_invalidation(
        thread_id,
        manager,
        thread_state,
        outgoing,
        parent_listener_generation,
        turn_id,
    )
    .await
}
