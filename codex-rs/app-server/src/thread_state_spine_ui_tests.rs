use super::*;
use crate::outgoing_message::OutgoingEnvelope;
use crate::outgoing_message::OutgoingMessage;
use crate::outgoing_message::OutgoingMessageSender;
use crate::spine_ui::listener::Command;
use codex_app_server_protocol::McpToolCallStatus;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ThreadItem;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::AgentStatus;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::SpineSpawnTaskProgress;
use codex_protocol::protocol::SpineTreeNodeKind;
use codex_protocol::protocol::SpineTreeNodeSnapshot;
use codex_protocol::protocol::SpineTreeNodeStatus;
use codex_protocol::protocol::TurnAbortReason;
use codex_protocol::protocol::TurnAbortedEvent;
use codex_protocol::protocol::TurnCompleteEvent;
use codex_protocol::protocol::TurnStartedEvent;
use serde_json::json;
use std::time::Duration;

fn tree_snapshot(sequence: u64, active_node_id: &str, changed: bool) -> SpineTreeUpdateEvent {
    let mut nodes = vec![SpineTreeNodeSnapshot {
        node_id: "1".to_string(),
        parent_id: None,
        kind: SpineTreeNodeKind::RootEpoch,
        status: SpineTreeNodeStatus::Opened,
        summary: None,
        memory_summary: Some("internal".to_string()),
        spawn_outcome: None,
        start: 0,
        end: Some(10),
        context_pressure: None,
    }];
    nodes.push(SpineTreeNodeSnapshot {
        node_id: "1.1".to_string(),
        parent_id: Some("1".to_string()),
        kind: SpineTreeNodeKind::Task,
        status: if changed {
            SpineTreeNodeStatus::Closed
        } else {
            SpineTreeNodeStatus::Live
        },
        summary: Some("first task".to_string()),
        memory_summary: Some("internal".to_string()),
        spawn_outcome: None,
        start: 1,
        end: Some(11),
        context_pressure: None,
    });
    if active_node_id == "1.2" {
        nodes.push(SpineTreeNodeSnapshot {
            node_id: "1.2".to_string(),
            parent_id: Some("1".to_string()),
            kind: SpineTreeNodeKind::Task,
            status: SpineTreeNodeStatus::Live,
            summary: Some("second task".to_string()),
            memory_summary: None,
            spawn_outcome: None,
            start: 2,
            end: None,
            context_pressure: None,
        });
    }
    SpineTreeUpdateEvent {
        snapshot_seq: sequence,
        active_node_id: active_node_id.to_string(),
        nodes,
        settled_spawn_call_ids: Vec::new(),
    }
}

fn growing_tree_snapshot(sequence: u64, task_count: u64) -> SpineTreeUpdateEvent {
    let mut nodes = vec![SpineTreeNodeSnapshot {
        node_id: "1".to_string(),
        parent_id: None,
        kind: SpineTreeNodeKind::RootEpoch,
        status: SpineTreeNodeStatus::Opened,
        summary: None,
        memory_summary: None,
        spawn_outcome: None,
        start: 0,
        end: None,
        context_pressure: None,
    }];
    nodes.extend((1..=task_count).map(|ordinal| SpineTreeNodeSnapshot {
        node_id: format!("1.{ordinal}"),
        parent_id: Some("1".to_string()),
        kind: SpineTreeNodeKind::Task,
        status: SpineTreeNodeStatus::Live,
        summary: Some(format!("task {ordinal}")),
        memory_summary: None,
        spawn_outcome: None,
        start: ordinal,
        end: None,
        context_pressure: None,
    }));
    SpineTreeUpdateEvent {
        snapshot_seq: sequence,
        active_node_id: format!("1.{task_count}"),
        nodes,
        settled_spawn_call_ids: Vec::new(),
    }
}

fn turn_started(turn_id: &str) -> EventMsg {
    EventMsg::TurnStarted(TurnStartedEvent {
        turn_id: turn_id.to_string(),
        trace_id: None,
        started_at: None,
        model_context_window: None,
        collaboration_mode_kind: Default::default(),
    })
}

fn spine_call() -> EventMsg {
    EventMsg::RawResponseItem(codex_protocol::protocol::RawResponseItemEvent {
        item: ResponseItem::FunctionCall {
            id: None,
            name: "open".to_string(),
            namespace: Some("spine".to_string()),
            arguments: "{}".to_string(),
            call_id: "call-spine".to_string(),
            internal_chat_message_metadata_passthrough: None,
        },
    })
}

fn code_mode_spine_carrier(tool: &str) -> EventMsg {
    const CARRIER_MARKER: &str = "spine.code_mode.output.v1";

    EventMsg::RawResponseItem(codex_protocol::protocol::RawResponseItemEvent {
        item: ResponseItem::CustomToolCallOutput {
            id: None,
            call_id: "code-mode-carrier".to_string(),
            name: Some(CARRIER_MARKER.to_string()),
            output: FunctionCallOutputPayload::from_text(
                json!({
                    "schema": CARRIER_MARKER,
                    "nested_spine_calls": [{"name": tool}],
                })
                .to_string(),
            ),
            internal_chat_message_metadata_passthrough: None,
        },
    })
}

fn turn_complete(turn_id: &str) -> EventMsg {
    EventMsg::TurnComplete(TurnCompleteEvent {
        turn_id: turn_id.to_string(),
        last_agent_message: None,
        completed_at: None,
        duration_ms: None,
        time_to_first_token_ms: None,
    })
}

fn turn_aborted(turn_id: &str, reason: TurnAbortReason) -> EventMsg {
    EventMsg::TurnAborted(TurnAbortedEvent {
        turn_id: Some(turn_id.to_string()),
        reason,
        completed_at: None,
        duration_ms: None,
    })
}

fn spawn_progress(child_thread_id: ThreadId) -> SpineSpawnProgressEvent {
    SpineSpawnProgressEvent {
        call_id: format!("spawn-{child_thread_id}"),
        tasks: vec![SpineSpawnTaskProgress {
            ordinal: 0,
            summary: "Run child agent".to_string(),
            thread_id: child_thread_id,
            agent_path: None,
            status: AgentStatus::Running,
        }],
    }
}

fn activate_spine_turn(state: &mut ThreadState, turn_id: &str, sequence: u64) {
    track_spine_ui_event(state, turn_id, &turn_started(turn_id));
    track_spine_ui_event(state, turn_id, &spine_call());
    state.record_spine_ui_snapshot(tree_snapshot(sequence, "1.1", false));
}

fn finish_spine_turn(state: &mut ThreadState, turn_id: &str) {
    track_spine_ui_event(state, turn_id, &turn_complete(turn_id));
    state.take_turn_summary();
}

fn track_spine_ui_event(state: &mut ThreadState, turn_id: &str, event: &EventMsg) {
    state.track_current_turn_event(turn_id, event);
    state.observe_spine_ui_event(turn_id, event);
}

async fn register_test_route(
    manager: &ThreadStateManager,
    parent_thread_id: ThreadId,
    parent_turn_id: &str,
    child_thread_id: ThreadId,
) -> SpineSpawnProgressEvent {
    let parent_listener_generation = manager
        .spine_ui_listener_generation_for_test(parent_thread_id)
        .await;
    let progress = spawn_progress(child_thread_id);
    let parent_state = manager.thread_state(parent_thread_id).await;
    {
        let mut state = parent_state.lock().await;
        activate_spine_turn(&mut state, parent_turn_id, 1);
        state.record_spine_ui_spawn_progress(progress.clone());
    }
    manager
        .register_spine_ui_spawn_progress(
            parent_thread_id,
            parent_listener_generation,
            parent_turn_id,
            &progress,
        )
        .await;
    progress
}

async fn recv_spine_ui_command(
    receiver: &mut mpsc::UnboundedReceiver<ThreadListenerCommand>,
    context: &str,
) -> Command {
    let ThreadListenerCommand::SpineUi(command) = receiver.recv().await.expect(context) else {
        panic!("{context}");
    };
    *command
}

async fn assert_bidirectional_routes(
    manager: &ThreadStateManager,
    parent_thread_id: ThreadId,
    current_thread_id: ThreadId,
    grandchild_thread_id: ThreadId,
    expected: bool,
) {
    let state = manager.state.lock().await;
    let incoming_exists = state
        .spine_ui
        .parent_by_child
        .get(&current_thread_id)
        .is_some_and(|route| route.parent_thread_id == parent_thread_id);
    let outgoing_exists = state
        .spine_ui
        .parent_by_child
        .get(&grandchild_thread_id)
        .is_some_and(|route| route.parent_thread_id == current_thread_id);
    assert_eq!((incoming_exists, outgoing_exists), (expected, expected));
}

async fn acknowledge_test_terminal(
    manager: &ThreadStateManager,
    thread_id: ThreadId,
    listener_generation: u64,
    turn_id: &str,
) {
    manager
        .acknowledge_spine_ui_agent_terminal(thread_id, listener_generation, turn_id)
        .await;
}

#[test]
fn each_turn_keeps_the_complete_tree() {
    let mut state = ThreadState::default();
    track_spine_ui_event(&mut state, "turn-1", &turn_started("turn-1"));
    track_spine_ui_event(&mut state, "turn-1", &spine_call());
    state.record_spine_ui_snapshot(tree_snapshot(1, "1.1", false));
    let first = state.spine_ui.active.clone();
    state.take_turn_summary();
    assert_eq!(first.latest_snapshot().unwrap().nodes.len(), 2);

    track_spine_ui_event(&mut state, "turn-2", &turn_started("turn-2"));
    track_spine_ui_event(&mut state, "turn-2", &spine_call());
    assert!(state.spine_ui.active.latest_snapshot().is_none());
    state.record_spine_ui_snapshot(tree_snapshot(1, "1.2", false));

    let second = state
        .spine_ui
        .active
        .latest_snapshot()
        .expect("second turn snapshot");
    let ids = second
        .nodes
        .iter()
        .map(|node| node.node_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["1", "1.1", "1.2"]);
    assert_eq!(second.active_node_id, "1.2");
}

#[test]
fn a_changed_old_node_is_kept_in_the_complete_tree() {
    let mut state = ThreadState::default();
    track_spine_ui_event(&mut state, "turn-1", &turn_started("turn-1"));
    track_spine_ui_event(&mut state, "turn-1", &spine_call());
    state.record_spine_ui_snapshot(tree_snapshot(1, "1.1", false));
    state.take_turn_summary();

    track_spine_ui_event(&mut state, "turn-2", &turn_started("turn-2"));
    track_spine_ui_event(&mut state, "turn-2", &spine_call());
    state.record_spine_ui_snapshot(tree_snapshot(2, "1.1", true));

    let second = state
        .spine_ui
        .active
        .latest_snapshot()
        .expect("second turn snapshot");
    assert_eq!(second.nodes.len(), 2);
    assert_eq!(second.nodes[1].node_id, "1.1");
    assert_eq!(second.nodes[1].status, SpineTreeNodeStatus::Closed);
}

#[test]
fn snapshot_without_an_active_spine_turn_seeds_the_next_complete_tree() {
    let mut state = ThreadState::default();
    state.record_spine_ui_snapshot(tree_snapshot(1, "1.1", false));
    assert!(state.live_spine_ui("turn-1").is_none());

    track_spine_ui_event(&mut state, "turn-1", &turn_started("turn-1"));
    track_spine_ui_event(&mut state, "turn-1", &spine_call());
    state.record_spine_ui_snapshot(tree_snapshot(2, "1.2", false));
    let snapshot = state
        .spine_ui
        .active
        .latest_snapshot()
        .expect("live snapshot");
    assert_eq!(snapshot.nodes.len(), 3);
    assert_eq!(snapshot.active_node_id, "1.2");
}

#[test]
fn every_live_card_contains_the_complete_tree_across_many_turns() {
    let mut state = ThreadState::default();
    let mut total_node_count = 0;

    for turn in 1..=32 {
        let turn_id = format!("turn-{turn}");
        track_spine_ui_event(&mut state, &turn_id, &turn_started(&turn_id));
        track_spine_ui_event(&mut state, &turn_id, &spine_call());
        state.record_spine_ui_snapshot(growing_tree_snapshot(turn, turn));

        let node_count = state
            .spine_ui
            .active
            .latest_snapshot()
            .expect("complete snapshot")
            .nodes
            .len();
        assert_eq!(node_count, turn as usize + 1);
        total_node_count += node_count;

        finish_spine_turn(&mut state, &turn_id);
    }

    assert_eq!(total_node_count, 560);
}

#[test]
fn a_turn_without_spine_does_not_discard_the_next_complete_tree() {
    let mut state = ThreadState::default();
    track_spine_ui_event(&mut state, "turn-1", &turn_started("turn-1"));
    track_spine_ui_event(&mut state, "turn-1", &spine_call());
    state.record_spine_ui_snapshot(tree_snapshot(1, "1.1", false));
    finish_spine_turn(&mut state, "turn-1");

    track_spine_ui_event(&mut state, "turn-2", &turn_started("turn-2"));
    finish_spine_turn(&mut state, "turn-2");

    track_spine_ui_event(&mut state, "turn-3", &turn_started("turn-3"));
    track_spine_ui_event(&mut state, "turn-3", &spine_call());
    state.record_spine_ui_snapshot(tree_snapshot(1, "1.2", false));

    let snapshot = state
        .spine_ui
        .active
        .latest_snapshot()
        .expect("complete tree after an ordinary turn");
    assert_eq!(snapshot.nodes.len(), 3);
    assert_eq!(snapshot.active_node_id, "1.2");
}

#[test]
fn spawned_agent_subtrees_are_carried_into_the_next_complete_tree() {
    let mut state = ThreadState::default();
    let child_thread_id = ThreadId::new();

    track_spine_ui_event(&mut state, "turn-1", &turn_started("turn-1"));
    track_spine_ui_event(&mut state, "turn-1", &spine_call());
    state.record_spine_ui_snapshot(tree_snapshot(1, "1.1", false));
    assert!(state.record_spine_ui_spawn_progress(spawn_progress(child_thread_id)));

    let mut child_state = SpineUiState::default();
    child_state.record_snapshot(tree_snapshot(1, "1.1", false));
    assert!(state.record_spine_ui_agent_state(child_thread_id, 1, child_state));
    finish_spine_turn(&mut state, "turn-1");

    track_spine_ui_event(&mut state, "turn-2", &turn_started("turn-2"));
    track_spine_ui_event(&mut state, "turn-2", &spine_call());
    state.record_spine_ui_snapshot(tree_snapshot(1, "1.2", false));

    let content = state
        .spine_ui
        .active
        .structured_content()
        .expect("second complete tree");
    assert_eq!(content["snapshot"]["nodes"].as_array().unwrap().len(), 3);
    assert_eq!(content["spawnCalls"].as_array().unwrap().len(), 1);
    assert_eq!(content["agentSubtrees"].as_array().unwrap().len(), 1);
    assert_eq!(
        content["agentSubtrees"][0]["threadId"],
        serde_json::json!(child_thread_id)
    );
}

#[test]
fn clearing_the_listener_drops_the_in_memory_complete_tree() {
    let mut state = ThreadState::default();
    track_spine_ui_event(&mut state, "turn-1", &turn_started("turn-1"));
    track_spine_ui_event(&mut state, "turn-1", &spine_call());
    state.record_spine_ui_snapshot(tree_snapshot(1, "1.1", false));
    finish_spine_turn(&mut state, "turn-1");

    state.clear_listener();
    track_spine_ui_event(&mut state, "turn-2", &turn_started("turn-2"));
    track_spine_ui_event(&mut state, "turn-2", &spine_call());

    assert!(state.spine_ui.active.latest_snapshot().is_none());
}

#[test]
fn replacing_the_listener_drops_the_in_memory_complete_tree() {
    let mut state = ThreadState::default();
    track_spine_ui_event(&mut state, "turn-1", &turn_started("turn-1"));
    track_spine_ui_event(&mut state, "turn-1", &spine_call());
    state.record_spine_ui_snapshot(tree_snapshot(1, "1.1", false));
    finish_spine_turn(&mut state, "turn-1");

    let (first_cancel_tx, mut first_cancel_rx) = oneshot::channel();
    state.cancel_tx = Some(first_cancel_tx);
    assert!(
        state
            .spine_ui
            .runtime
            .cumulative
            .latest_snapshot()
            .is_some()
    );

    let (replacement_cancel_tx, _replacement_cancel_rx) = oneshot::channel();
    if let Some(previous) = state.cancel_tx.replace(replacement_cancel_tx) {
        let _ = previous.send(());
    }
    state.spine_ui.clear_transient_state();
    assert_eq!(first_cancel_rx.try_recv(), Ok(()));

    track_spine_ui_event(&mut state, "turn-2", &turn_started("turn-2"));
    track_spine_ui_event(&mut state, "turn-2", &spine_call());
    assert!(state.spine_ui.active.latest_snapshot().is_none());
}

#[tokio::test]
async fn forwarded_agent_state_excludes_nodes_inherited_from_the_parent() {
    let manager = ThreadStateManager::new();
    let parent_thread_id = ThreadId::new();
    let child_thread_id = ThreadId::new();
    let child_state = manager.thread_state(child_thread_id).await;
    {
        let mut state = child_state.lock().await;
        track_spine_ui_event(&mut state, "child-turn", &turn_started("child-turn"));
        track_spine_ui_event(&mut state, "child-turn", &spine_call());
        let mut child_snapshot = tree_snapshot(2, "1.1", false);
        child_snapshot.active_node_id = "1.1.1".to_string();
        child_snapshot.nodes.push(SpineTreeNodeSnapshot {
            node_id: "1.1.1".to_string(),
            parent_id: Some("1.1".to_string()),
            kind: SpineTreeNodeKind::Task,
            status: SpineTreeNodeStatus::Live,
            summary: Some("child-only task".to_string()),
            memory_summary: None,
            spawn_outcome: None,
            start: 2,
            end: None,
            context_pressure: None,
        });
        state.record_spine_ui_snapshot(child_snapshot);
    }
    let (listener_tx, mut listener_rx) = mpsc::unbounded_channel();
    manager.register_listener_command_tx(parent_thread_id, listener_tx);

    register_test_route(&manager, parent_thread_id, "parent-turn", child_thread_id).await;

    let Command::ForwardSpineUiAgentState {
        state: Some(forwarded),
        ..
    } = recv_spine_ui_command(&mut listener_rx, "initial child state").await
    else {
        panic!("expected initial child state");
    };
    let forwarded_node_ids = forwarded
        .latest_snapshot()
        .expect("forwarded child snapshot")
        .nodes
        .iter()
        .map(|node| node.node_id.as_str())
        .collect::<Vec<_>>();

    assert_eq!(forwarded_node_ids, vec!["1.1.1"]);
}

#[tokio::test]
async fn route_baseline_uses_cumulative_tree_before_current_snapshot_exists() {
    let manager = ThreadStateManager::new();
    let parent_thread_id = ThreadId::new();
    let child_thread_id = ThreadId::new();
    let parent_state = manager.thread_state(parent_thread_id).await;
    {
        let mut state = parent_state.lock().await;
        activate_spine_turn(&mut state, "previous-turn", 1);
        finish_spine_turn(&mut state, "previous-turn");

        track_spine_ui_event(&mut state, "parent-turn", &turn_started("parent-turn"));
        track_spine_ui_event(&mut state, "parent-turn", &spine_call());
        assert!(state.spine_ui.active.latest_snapshot().is_none());
        assert!(
            state
                .spine_ui
                .runtime
                .cumulative
                .latest_snapshot()
                .is_some()
        );
        assert!(state.record_spine_ui_spawn_progress(spawn_progress(child_thread_id)));
    }

    let child_state = manager.thread_state(child_thread_id).await;
    {
        let mut state = child_state.lock().await;
        track_spine_ui_event(&mut state, "child-turn", &turn_started("child-turn"));
        track_spine_ui_event(&mut state, "child-turn", &spine_call());
        let mut child_snapshot = tree_snapshot(2, "1.1.1", false);
        child_snapshot.nodes.push(SpineTreeNodeSnapshot {
            node_id: "1.1.1".to_string(),
            parent_id: Some("1.1".to_string()),
            kind: SpineTreeNodeKind::Task,
            status: SpineTreeNodeStatus::Live,
            summary: Some("child-only task".to_string()),
            memory_summary: None,
            spawn_outcome: None,
            start: 2,
            end: None,
            context_pressure: None,
        });
        state.record_spine_ui_snapshot(child_snapshot);
    }

    let (listener_tx, mut listener_rx) = mpsc::unbounded_channel();
    manager.register_listener_command_tx(parent_thread_id, listener_tx);
    let progress = spawn_progress(child_thread_id);
    manager
        .register_spine_ui_spawn_progress(parent_thread_id, 0, "parent-turn", &progress)
        .await;

    let Command::ForwardSpineUiAgentState {
        state: Some(forwarded),
        ..
    } = recv_spine_ui_command(&mut listener_rx, "initial child state").await
    else {
        panic!("expected initial child state");
    };
    let forwarded_node_ids = forwarded
        .latest_snapshot()
        .expect("forwarded child snapshot")
        .nodes
        .iter()
        .map(|node| node.node_id.as_str())
        .collect::<Vec<_>>();

    assert_eq!(forwarded_node_ids, vec!["1.1.1"]);
}

#[test]
fn route_baseline_is_empty_before_the_first_snapshot() {
    let mut state = ThreadState::default();
    track_spine_ui_event(&mut state, "turn-1", &turn_started("turn-1"));
    track_spine_ui_event(&mut state, "turn-1", &spine_call());

    assert_eq!(
        state.spine_ui_baseline_node_ids("turn-1"),
        Some(HashSet::new())
    );
}

#[tokio::test]
async fn repeated_progress_is_deduplicated_and_newer_states_are_coalesced() {
    let manager = ThreadStateManager::new();
    let parent_thread_id = ThreadId::new();
    let child_thread_id = ThreadId::new();
    let child_state = manager.thread_state(child_thread_id).await;
    {
        let mut state = child_state.lock().await;
        activate_spine_turn(&mut state, "child-turn", 2);
    }
    let (listener_tx, mut listener_rx) = mpsc::unbounded_channel();
    manager.register_listener_command_tx(parent_thread_id, listener_tx);

    let progress =
        register_test_route(&manager, parent_thread_id, "parent-turn", child_thread_id).await;
    let Command::ForwardSpineUiAgentState {
        generation,
        state: Some(initial_state),
        ..
    } = recv_spine_ui_command(&mut listener_rx, "initial child state").await
    else {
        panic!("expected initial child state");
    };
    manager
        .complete_spine_ui_agent_state_forward(
            child_thread_id,
            parent_thread_id,
            "parent-turn",
            generation,
            initial_state.revision(),
        )
        .await;

    manager
        .register_spine_ui_spawn_progress(parent_thread_id, 0, "parent-turn", &progress)
        .await;
    manager.queue_spine_ui_agent_state(child_thread_id).await;

    assert!(matches!(
        listener_rx.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));

    {
        let mut state = child_state.lock().await;
        state.record_spine_ui_snapshot(tree_snapshot(3, "1.2", false));
    }
    manager.queue_spine_ui_agent_state(child_thread_id).await;
    manager.queue_spine_ui_agent_state(child_thread_id).await;
    let Command::ForwardSpineUiAgentState {
        state: Some(forwarded),
        ..
    } = recv_spine_ui_command(&mut listener_rx, "updated child state").await
    else {
        panic!("expected updated child state");
    };
    {
        let mut state = child_state.lock().await;
        state.record_spine_ui_snapshot(tree_snapshot(4, "1.1", false));
    }
    manager.queue_spine_ui_agent_state(child_thread_id).await;
    manager.queue_spine_ui_agent_state(child_thread_id).await;
    assert!(matches!(
        listener_rx.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    manager
        .complete_spine_ui_agent_state_forward(
            child_thread_id,
            parent_thread_id,
            "parent-turn",
            generation,
            forwarded.revision(),
        )
        .await;
    let Command::ForwardSpineUiAgentState {
        state: Some(coalesced),
        ..
    } = recv_spine_ui_command(&mut listener_rx, "coalesced latest child state").await
    else {
        panic!("expected coalesced latest child state");
    };
    assert_eq!(
        coalesced
            .latest_snapshot()
            .expect("coalesced snapshot")
            .snapshot_seq,
        4
    );
}

#[test]
fn spawn_progress_activates_a_turn_without_a_direct_spine_tool_call() {
    let mut state = ThreadState::default();
    let child_thread_id = ThreadId::new();
    track_spine_ui_event(&mut state, "turn-1", &turn_started("turn-1"));
    track_spine_ui_event(
        &mut state,
        "turn-1",
        &EventMsg::SpineSpawnProgress(spawn_progress(child_thread_id)),
    );

    assert!(state.record_spine_ui_spawn_progress(spawn_progress(child_thread_id)));
    assert!(state.record_spine_ui_snapshot(tree_snapshot(1, "1.1", false)));
    assert!(state.live_spine_ui("turn-1").is_some());
}

#[test]
fn code_mode_tree_control_activates_the_turn_before_its_snapshot() {
    let mut state = ThreadState::default();
    track_spine_ui_event(&mut state, "turn-1", &turn_started("turn-1"));
    track_spine_ui_event(&mut state, "turn-1", &code_mode_spine_carrier("close"));

    assert!(state.record_spine_ui_snapshot(tree_snapshot(1, "1", true)));
    assert!(state.live_spine_ui("turn-1").is_some());
}

#[test]
fn code_mode_trim_keeps_the_snapshot_cumulative_without_opening_a_card() {
    let mut state = ThreadState::default();
    track_spine_ui_event(&mut state, "turn-1", &turn_started("turn-1"));
    track_spine_ui_event(&mut state, "turn-1", &code_mode_spine_carrier("trim"));

    assert!(!state.record_spine_ui_snapshot(tree_snapshot(1, "1", true)));
    assert!(state.live_spine_ui("turn-1").is_none());
    assert!(
        state
            .spine_ui
            .runtime
            .cumulative
            .latest_snapshot()
            .is_some()
    );
}

#[tokio::test]
async fn stale_turn_started_does_not_remove_routes_owned_by_a_new_listener() {
    let manager = ThreadStateManager::new();
    let parent_thread_id = ThreadId::new();
    let child_thread_id = ThreadId::new();
    let child_state = manager.thread_state(child_thread_id).await;
    {
        let mut child_state = child_state.lock().await;
        activate_spine_turn(&mut child_state, "child-turn", 1);
    }
    manager
        .note_spine_ui_listener_generation(parent_thread_id, 2)
        .await;
    let (listener_tx, mut listener_rx) = mpsc::unbounded_channel();
    manager.register_listener_command_tx(parent_thread_id, listener_tx);
    register_test_route(&manager, parent_thread_id, "parent-turn", child_thread_id).await;
    let Command::ForwardSpineUiAgentState { generation, .. } =
        recv_spine_ui_command(&mut listener_rx, "initial child state").await
    else {
        panic!("expected initial child state");
    };

    manager
        .note_spine_ui_agent_turn_started(parent_thread_id, 1, "stale-parent-turn")
        .await;

    assert!(
        !manager
            .spine_ui_route_is_current(
                child_thread_id,
                parent_thread_id,
                "parent-turn",
                1,
                generation,
            )
            .await
    );
    assert!(
        manager
            .spine_ui_route_is_current(
                child_thread_id,
                parent_thread_id,
                "parent-turn",
                2,
                generation,
            )
            .await
    );
}

#[test]
fn pending_completion_survives_the_next_turn_and_completes_once() {
    let mut state = ThreadState::default();
    let child_thread_id = ThreadId::new();
    activate_spine_turn(&mut state, "turn-1", 1);
    assert!(state.record_spine_ui_spawn_progress(spawn_progress(child_thread_id)));
    state.mark_spine_ui_mounted("turn-1", &[ConnectionId(7)]);
    track_spine_ui_event(&mut state, "turn-1", &turn_complete("turn-1"));
    let completion_token = state
        .begin_spine_ui_turn_completion("turn-1")
        .expect("first turn should begin completion");

    track_spine_ui_event(&mut state, "turn-2", &turn_started("turn-2"));
    track_spine_ui_event(&mut state, "turn-2", &spine_call());
    assert!(state.record_spine_ui_snapshot(tree_snapshot(2, "1.2", false)));
    let mounted = state
        .mount_spine_ui_for_connection(ConnectionId(8))
        .expect("active second turn should mount");
    assert_eq!(mounted.0, "turn-2");

    let completion = state
        .finalize_spine_ui_turn("turn-1", completion_token)
        .expect("first turn completion");
    assert_eq!(completion.connection_ids, vec![ConnectionId(7)]);
    assert!(completion.state.completed_at_ms().is_some());
    assert_eq!(
        completion
            .state
            .structured_content()
            .expect("completed content")["spawnCalls"][0]["tasks"][0]["status"],
        serde_json::json!("running")
    );
    assert!(
        state
            .finalize_spine_ui_turn("turn-1", completion_token)
            .is_none()
    );
    assert_eq!(
        state
            .active_spine_ui_snapshot()
            .expect("second turn remains active")
            .1
            .latest_snapshot()
            .expect("second turn snapshot")
            .snapshot_seq,
        2
    );
}

#[test]
fn interrupted_agents_remain_terminal_when_carried_into_the_next_turn() {
    let mut state = ThreadState::default();
    activate_spine_turn(&mut state, "turn-1", 1);
    assert!(state.record_spine_ui_spawn_progress(spawn_progress(ThreadId::new())));
    assert!(state.terminalize_spine_ui_incomplete_agents(AgentStatus::Interrupted));
    state
        .begin_spine_ui_turn_completion("turn-1")
        .expect("interrupted turn should begin completion");
    track_spine_ui_event(
        &mut state,
        "turn-1",
        &turn_aborted("turn-1", TurnAbortReason::Interrupted),
    );

    track_spine_ui_event(&mut state, "turn-2", &turn_started("turn-2"));
    track_spine_ui_event(&mut state, "turn-2", &spine_call());
    assert!(state.record_spine_ui_snapshot(tree_snapshot(2, "1.2", false)));

    let content = state
        .spine_ui
        .active
        .structured_content()
        .expect("carried-forward content");
    assert_eq!(
        content["spawnCalls"][0]["tasks"][0]["status"],
        "interrupted"
    );
}

#[test]
fn rollback_invalidates_a_pending_completion_token() {
    let mut state = ThreadState::default();
    activate_spine_turn(&mut state, "turn-1", 1);
    track_spine_ui_event(&mut state, "turn-1", &turn_complete("turn-1"));
    let completion_token = state
        .begin_spine_ui_turn_completion("turn-1")
        .expect("turn should begin completion");

    state.reset_spine_ui_after_rollback();

    assert!(
        state
            .finalize_spine_ui_turn("turn-1", completion_token)
            .is_none()
    );
}

#[test]
fn reconnect_mounts_a_pending_card_before_its_single_completion() {
    let mut state = ThreadState::default();
    activate_spine_turn(&mut state, "turn-1", 1);
    state.mark_spine_ui_mounted("turn-1", &[ConnectionId(7)]);
    track_spine_ui_event(&mut state, "turn-1", &turn_complete("turn-1"));
    let completion_token = state
        .begin_spine_ui_turn_completion("turn-1")
        .expect("turn should begin completion");

    let (turn_id, mounted) = state
        .mount_spine_ui_for_connection(ConnectionId(8))
        .expect("pending card should mount");
    assert_eq!(turn_id, "turn-1");
    assert!(mounted.latest_snapshot().is_some());

    let mut connection_ids = state
        .finalize_spine_ui_turn("turn-1", completion_token)
        .expect("pending completion")
        .connection_ids;
    connection_ids.sort_by_key(|connection_id| connection_id.0);
    assert_eq!(connection_ids, vec![ConnectionId(7), ConnectionId(8)]);
}

#[tokio::test]
async fn stale_listener_events_cannot_mutate_or_finalize_replacement_state() {
    let manager = ThreadStateManager::new();
    let parent_thread_id = ThreadId::new();
    let child_thread_id = ThreadId::new();
    let parent_state = manager.thread_state(parent_thread_id).await;
    let child_state = manager.thread_state(child_thread_id).await;
    {
        let mut state = parent_state.lock().await;
        state.listener_generation = 2;
        activate_spine_turn(&mut state, "parent-turn", 1);
        state.record_spine_ui_spawn_progress(spawn_progress(child_thread_id));
    }
    {
        let mut state = child_state.lock().await;
        activate_spine_turn(&mut state, "child-turn", 2);
    }
    manager
        .note_spine_ui_listener_generation(parent_thread_id, 2)
        .await;
    let (listener_tx, mut listener_rx) = mpsc::unbounded_channel();
    manager.register_listener_command_tx(parent_thread_id, listener_tx);
    register_test_route(&manager, parent_thread_id, "parent-turn", child_thread_id).await;
    let Command::ForwardSpineUiAgentState {
        generation: route_generation,
        ..
    } = recv_spine_ui_command(&mut listener_rx, "replacement child state").await
    else {
        panic!("expected replacement child state");
    };

    {
        let mut state = parent_state.lock().await;
        assert!(
            state
                .track_current_turn_event_for_listener(
                    1,
                    "stale-turn",
                    &turn_started("stale-turn"),
                )
                .is_none()
        );
        assert_eq!(
            state.active_turn_snapshot().map(|turn| turn.id),
            Some("parent-turn".to_string())
        );
    }

    for event in [
        Event {
            id: "stale-turn".to_string(),
            msg: turn_started("stale-turn"),
        },
        Event {
            id: "parent-turn".to_string(),
            msg: EventMsg::ThreadRolledBack(codex_protocol::protocol::ThreadRolledBackEvent {
                num_turns: 1,
            }),
        },
    ] {
        crate::spine_ui::listener::after_track_enabled_for_test(
            &manager,
            &parent_state,
            parent_thread_id,
            1,
            &event,
        )
        .await;
    }

    let (outgoing_tx, _outgoing_rx) = mpsc::channel(4);
    let outgoing = Arc::new(OutgoingMessageSender::new(
        outgoing_tx,
        codex_analytics::AnalyticsEventsClient::disabled(),
    ));
    let stale_terminal = Event {
        id: "parent-turn".to_string(),
        msg: turn_complete("parent-turn"),
    };
    assert!(
        crate::spine_ui::listener::before_bespoke_enabled_for_test(
            &outgoing,
            &[],
            &parent_state,
            parent_thread_id,
            1,
            &stale_terminal,
        )
        .await
        .is_none()
    );
    manager
        .clear_spine_ui_parent_routes(parent_thread_id, "parent-turn", 1)
        .await;

    assert_eq!(
        parent_state
            .lock()
            .await
            .current_spine_ui_for_forward()
            .and_then(SpineUiState::latest_snapshot)
            .map(|snapshot| snapshot.snapshot_seq),
        Some(1)
    );
    assert!(
        manager
            .spine_ui_route_is_current(
                child_thread_id,
                parent_thread_id,
                "parent-turn",
                2,
                route_generation,
            )
            .await
    );
}

#[tokio::test]
async fn stale_route_invalidation_cannot_mutate_or_refresh_a_replacement_listener() {
    let manager = ThreadStateManager::new();
    let parent_thread_id = ThreadId::new();
    let child_thread_id = ThreadId::new();
    let child_state = manager.thread_state(child_thread_id).await;
    {
        let mut state = child_state.lock().await;
        activate_spine_turn(&mut state, "child-turn", 2);
    }
    let (old_listener_tx, mut old_listener_rx) = mpsc::unbounded_channel();
    manager.register_listener_command_tx(parent_thread_id, old_listener_tx);
    register_test_route(&manager, parent_thread_id, "parent-turn", child_thread_id).await;
    let Command::ForwardSpineUiAgentState { .. } =
        recv_spine_ui_command(&mut old_listener_rx, "initial child state").await
    else {
        panic!("expected initial child state");
    };

    let parent_state = manager.thread_state(parent_thread_id).await;
    {
        let mut state = parent_state.lock().await;
        state.listener_generation = 1;
        activate_spine_turn(&mut state, "replacement-turn", 9);
    }
    let (replacement_tx, mut replacement_rx) = mpsc::unbounded_channel();
    manager.register_listener_command_tx(parent_thread_id, replacement_tx);

    manager
        .clear_all_spine_ui_routes_for_thread(child_thread_id)
        .await;
    assert!(matches!(
        replacement_rx.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    assert_eq!(
        parent_state
            .lock()
            .await
            .current_spine_ui_for_forward()
            .and_then(SpineUiState::latest_snapshot)
            .map(|snapshot| snapshot.snapshot_seq),
        Some(9)
    );

    let (outgoing_tx, _outgoing_rx) = mpsc::channel(4);
    let outgoing = Arc::new(OutgoingMessageSender::new(
        outgoing_tx,
        codex_analytics::AnalyticsEventsClient::disabled(),
    ));
    assert!(
        !crate::spine_ui::listener::emit_invalidation_enabled_for_test(
            parent_thread_id,
            &manager,
            &parent_state,
            &outgoing,
            0,
            "parent-turn".to_string(),
        )
        .await
    );
}

#[tokio::test]
async fn stale_listener_exit_cannot_clear_replacement_routes_before_generation_is_mirrored() {
    let manager = ThreadStateManager::new();
    let parent_thread_id = ThreadId::new();
    let replaced_thread_id = ThreadId::new();
    let grandchild_thread_id = ThreadId::new();
    let replaced_state = manager.thread_state(replaced_thread_id).await;
    let grandchild_state = manager.thread_state(grandchild_thread_id).await;
    manager
        .note_spine_ui_listener_generation(replaced_thread_id, 1)
        .await;
    {
        let mut state = replaced_state.lock().await;
        state.listener_generation = 1;
        activate_spine_turn(&mut state, "replaced-turn", 1);
    }
    {
        let mut state = grandchild_state.lock().await;
        activate_spine_turn(&mut state, "grandchild-turn", 1);
    }

    let (parent_tx, mut parent_rx) = mpsc::unbounded_channel();
    manager.register_listener_command_tx(parent_thread_id, parent_tx);
    let (replaced_tx, mut replaced_rx) = mpsc::unbounded_channel();
    manager.register_listener_command_tx(replaced_thread_id, replaced_tx);

    register_test_route(
        &manager,
        parent_thread_id,
        "parent-turn",
        replaced_thread_id,
    )
    .await;
    let Command::ForwardSpineUiAgentState {
        parent_listener_generation: incoming_parent_generation,
        generation: incoming_route_generation,
        ..
    } = recv_spine_ui_command(&mut parent_rx, "incoming route state").await
    else {
        panic!("expected incoming route state");
    };

    register_test_route(
        &manager,
        replaced_thread_id,
        "replaced-parent-turn",
        grandchild_thread_id,
    )
    .await;
    let Command::ForwardSpineUiAgentState {
        parent_listener_generation: outgoing_parent_generation,
        generation: outgoing_route_generation,
        ..
    } = recv_spine_ui_command(&mut replaced_rx, "outgoing route state").await
    else {
        panic!("expected outgoing route state");
    };

    // Reproduce the publication window: ThreadState is authoritative generation
    // 2 while the manager mirror still reports generation 1.
    replaced_state.lock().await.listener_generation = 2;
    assert_eq!(
        manager
            .spine_ui_listener_generation_for_test(replaced_thread_id)
            .await,
        1
    );

    crate::spine_ui::listener::listener_exited_enabled_for_test(
        &manager,
        &replaced_state,
        replaced_thread_id,
        1,
    )
    .await;

    assert!(
        manager
            .spine_ui_route_is_current(
                replaced_thread_id,
                parent_thread_id,
                "parent-turn",
                incoming_parent_generation,
                incoming_route_generation,
            )
            .await
    );
    assert!(
        manager
            .spine_ui_route_is_current(
                grandchild_thread_id,
                replaced_thread_id,
                "replaced-parent-turn",
                outgoing_parent_generation,
                outgoing_route_generation,
            )
            .await
    );
}

#[tokio::test]
async fn current_listener_exit_still_clears_incoming_and_outgoing_routes() {
    let manager = ThreadStateManager::new();
    let parent_thread_id = ThreadId::new();
    let current_thread_id = ThreadId::new();
    let grandchild_thread_id = ThreadId::new();
    let current_state = manager.thread_state(current_thread_id).await;
    let grandchild_state = manager.thread_state(grandchild_thread_id).await;
    manager
        .note_spine_ui_listener_generation(current_thread_id, 1)
        .await;
    {
        let mut state = current_state.lock().await;
        state.listener_generation = 1;
        activate_spine_turn(&mut state, "current-turn", 1);
    }
    {
        let mut state = grandchild_state.lock().await;
        activate_spine_turn(&mut state, "grandchild-turn", 1);
    }

    let (parent_tx, mut parent_rx) = mpsc::unbounded_channel();
    manager.register_listener_command_tx(parent_thread_id, parent_tx);
    let (current_tx, mut current_rx) = mpsc::unbounded_channel();
    manager.register_listener_command_tx(current_thread_id, current_tx);

    register_test_route(&manager, parent_thread_id, "parent-turn", current_thread_id).await;
    let Command::ForwardSpineUiAgentState {
        parent_listener_generation: incoming_parent_generation,
        generation: incoming_route_generation,
        ..
    } = recv_spine_ui_command(&mut parent_rx, "incoming route state").await
    else {
        panic!("expected incoming route state");
    };

    register_test_route(
        &manager,
        current_thread_id,
        "current-parent-turn",
        grandchild_thread_id,
    )
    .await;
    let Command::ForwardSpineUiAgentState {
        parent_listener_generation: outgoing_parent_generation,
        generation: outgoing_route_generation,
        ..
    } = recv_spine_ui_command(&mut current_rx, "outgoing route state").await
    else {
        panic!("expected outgoing route state");
    };

    assert!(
        manager
            .spine_ui_route_is_current(
                current_thread_id,
                parent_thread_id,
                "parent-turn",
                incoming_parent_generation,
                incoming_route_generation,
            )
            .await
    );
    assert!(
        manager
            .spine_ui_route_is_current(
                grandchild_thread_id,
                current_thread_id,
                "current-parent-turn",
                outgoing_parent_generation,
                outgoing_route_generation,
            )
            .await
    );

    crate::spine_ui::listener::listener_exited_enabled_for_test(
        &manager,
        &current_state,
        current_thread_id,
        1,
    )
    .await;

    assert!(
        !manager
            .spine_ui_route_is_current(
                current_thread_id,
                parent_thread_id,
                "parent-turn",
                incoming_parent_generation,
                incoming_route_generation,
            )
            .await
    );
    assert!(
        !manager
            .spine_ui_route_is_current(
                grandchild_thread_id,
                current_thread_id,
                "current-parent-turn",
                outgoing_parent_generation,
                outgoing_route_generation,
            )
            .await
    );
}

#[tokio::test]
async fn stale_thread_state_instance_cannot_clear_reloaded_thread_routes() {
    let manager = ThreadStateManager::new();
    let parent_thread_id = ThreadId::new();
    let reloaded_thread_id = ThreadId::new();
    let grandchild_thread_id = ThreadId::new();

    let old_state = manager.thread_state(reloaded_thread_id).await;
    old_state.lock().await.listener_generation = 1;
    manager
        .note_spine_ui_listener_generation(reloaded_thread_id, 1)
        .await;
    manager.remove_thread_state(reloaded_thread_id).await;

    let new_state = manager.thread_state(reloaded_thread_id).await;
    assert!(!Arc::ptr_eq(&old_state, &new_state));
    {
        let mut state = new_state.lock().await;
        state.listener_generation = 1;
        activate_spine_turn(&mut state, "reloaded-turn", 1);
    }
    manager
        .note_spine_ui_listener_generation(reloaded_thread_id, 1)
        .await;
    assert_eq!(old_state.lock().await.listener_generation, 1);
    assert_eq!(new_state.lock().await.listener_generation, 1);
    assert_eq!(
        manager
            .spine_ui_listener_generation_for_test(reloaded_thread_id)
            .await,
        1
    );
    let grandchild_state = manager.thread_state(grandchild_thread_id).await;
    {
        let mut state = grandchild_state.lock().await;
        activate_spine_turn(&mut state, "grandchild-turn", 1);
    }

    let (parent_tx, _parent_rx) = mpsc::unbounded_channel();
    manager.register_listener_command_tx(parent_thread_id, parent_tx);
    let (reloaded_tx, _reloaded_rx) = mpsc::unbounded_channel();
    manager.register_listener_command_tx(reloaded_thread_id, reloaded_tx);
    register_test_route(
        &manager,
        parent_thread_id,
        "parent-turn",
        reloaded_thread_id,
    )
    .await;
    register_test_route(
        &manager,
        reloaded_thread_id,
        "reloaded-parent-turn",
        grandchild_thread_id,
    )
    .await;
    assert_bidirectional_routes(
        &manager,
        parent_thread_id,
        reloaded_thread_id,
        grandchild_thread_id,
        true,
    )
    .await;

    crate::spine_ui::listener::listener_exited_enabled_for_test(
        &manager,
        &old_state,
        reloaded_thread_id,
        1,
    )
    .await;
    assert_bidirectional_routes(
        &manager,
        parent_thread_id,
        reloaded_thread_id,
        grandchild_thread_id,
        true,
    )
    .await;

    crate::spine_ui::listener::after_track_enabled_for_test(
        &manager,
        &old_state,
        reloaded_thread_id,
        1,
        &Event {
            id: "old-turn".to_string(),
            msg: EventMsg::ThreadRolledBack(codex_protocol::protocol::ThreadRolledBackEvent {
                num_turns: 1,
            }),
        },
    )
    .await;
    assert_bidirectional_routes(
        &manager,
        parent_thread_id,
        reloaded_thread_id,
        grandchild_thread_id,
        true,
    )
    .await;

    crate::spine_ui::listener::listener_exited_enabled_for_test(
        &manager,
        &new_state,
        reloaded_thread_id,
        1,
    )
    .await;
    assert_bidirectional_routes(
        &manager,
        parent_thread_id,
        reloaded_thread_id,
        grandchild_thread_id,
        false,
    )
    .await;
}

#[tokio::test]
async fn listener_exit_keeps_the_authoritative_generation_locked_through_route_cleanup() {
    let manager = ThreadStateManager::new();
    let parent_thread_id = ThreadId::new();
    let child_thread_id = ThreadId::new();
    let child_state = manager.thread_state(child_thread_id).await;
    manager
        .note_spine_ui_listener_generation(child_thread_id, 1)
        .await;
    {
        let mut state = child_state.lock().await;
        state.listener_generation = 1;
        activate_spine_turn(&mut state, "child-turn", 1);
    }
    let (parent_tx, mut parent_rx) = mpsc::unbounded_channel();
    manager.register_listener_command_tx(parent_thread_id, parent_tx);
    register_test_route(&manager, parent_thread_id, "parent-turn", child_thread_id).await;
    let Command::ForwardSpineUiAgentState {
        parent_listener_generation,
        generation: route_generation,
        ..
    } = recv_spine_ui_command(&mut parent_rx, "initial child state").await
    else {
        panic!("expected initial child state");
    };

    // Hold the manager lock so listener exit can only be blocked after it has
    // acquired the authoritative ThreadState lock.
    let manager_guard = manager.state.lock().await;
    let exit_manager = manager.clone();
    let exit_state = Arc::clone(&child_state);
    let exit_task = tokio::spawn(async move {
        crate::spine_ui::listener::listener_exited_enabled_for_test(
            &exit_manager,
            &exit_state,
            child_thread_id,
            1,
        )
        .await;
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            match child_state.try_lock() {
                Ok(state) => drop(state),
                Err(_) => break,
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("listener exit should acquire the authoritative ThreadState lock");

    let replacement_state = Arc::clone(&child_state);
    let replacement_task = tokio::spawn(async move {
        replacement_state.lock().await.listener_generation = 2;
    });
    tokio::task::yield_now().await;
    assert!(!replacement_task.is_finished());

    drop(manager_guard);
    exit_task.await.expect("listener exit task");
    replacement_task.await.expect("replacement task");
    assert_eq!(child_state.lock().await.listener_generation, 2);
    assert!(
        !manager
            .spine_ui_route_is_current(
                child_thread_id,
                parent_thread_id,
                "parent-turn",
                parent_listener_generation,
                route_generation,
            )
            .await
    );
}

#[tokio::test]
async fn child_listener_replacement_invalidates_old_projection_and_accepts_new_state() {
    let manager = ThreadStateManager::new();
    let parent_thread_id = ThreadId::new();
    let child_thread_id = ThreadId::new();
    let child_state = manager.thread_state(child_thread_id).await;
    {
        let mut child_state = child_state.lock().await;
        activate_spine_turn(&mut child_state, "child-turn", 1);
    }
    let (listener_tx, mut listener_rx) = mpsc::unbounded_channel();
    manager.register_listener_command_tx(parent_thread_id, listener_tx);
    register_test_route(&manager, parent_thread_id, "parent-turn", child_thread_id).await;
    let Command::ForwardSpineUiAgentState {
        parent_turn_id,
        parent_listener_generation,
        generation: route_generation,
        state: Some(old_child_state),
        ..
    } = recv_spine_ui_command(&mut listener_rx, "initial child state").await
    else {
        panic!("expected initial child state");
    };
    let parent_state = manager.thread_state(parent_thread_id).await;
    assert!(parent_state.lock().await.record_spine_ui_agent_state(
        child_thread_id,
        route_generation,
        old_child_state.clone(),
    ));

    manager
        .note_spine_ui_listener_generation(child_thread_id, 2)
        .await;
    let Command::EmitSpineUiInvalidation { .. } =
        recv_spine_ui_command(&mut listener_rx, "replacement invalidation").await
    else {
        panic!("expected replacement invalidation");
    };
    assert!(
        parent_state
            .lock()
            .await
            .current_spine_ui_for_forward()
            .and_then(SpineUiState::structured_content)
            .and_then(|content| content["agentSubtrees"].as_array().cloned())
            .is_none_or(|subtrees| subtrees.is_empty())
    );
    assert!(
        !manager
            .spine_ui_route_is_current(
                child_thread_id,
                parent_thread_id,
                "parent-turn",
                parent_listener_generation,
                route_generation,
            )
            .await
    );

    let (outgoing_tx, mut outgoing_rx) = mpsc::channel(4);
    let outgoing = Arc::new(OutgoingMessageSender::new(
        outgoing_tx,
        codex_analytics::AnalyticsEventsClient::disabled(),
    ));
    crate::spine_ui::listener::handle_command(
        parent_thread_id,
        &manager,
        &parent_state,
        &outgoing,
        ThreadListenerCommand::SpineUi(Box::new(Command::ForwardSpineUiAgentState {
            child_thread_id,
            parent_turn_id,
            parent_listener_generation,
            generation: route_generation,
            state: Some(old_child_state),
        })),
    )
    .await;
    assert!(outgoing_rx.try_recv().is_err());

    manager
        .clear_spine_ui_routes_for_listener_exit(child_thread_id, 1, &child_state)
        .await;
    manager
        .note_spine_ui_agent_turn_started(child_thread_id, 2, "child-turn")
        .await;
    {
        let mut child_state = child_state.lock().await;
        child_state.listener_generation = 2;
        assert!(child_state.record_spine_ui_snapshot(tree_snapshot(2, "1.1", true)));
    }
    manager.queue_spine_ui_agent_state(child_thread_id).await;
    let Command::ForwardSpineUiAgentState {
        generation: replacement_generation,
        state: Some(replacement_state),
        ..
    } = recv_spine_ui_command(&mut listener_rx, "replacement child state").await
    else {
        panic!("expected replacement child state");
    };
    assert!(replacement_generation > route_generation);
    assert!(parent_state.lock().await.record_spine_ui_agent_state(
        child_thread_id,
        replacement_generation,
        replacement_state,
    ));
    assert!(
        parent_state
            .lock()
            .await
            .current_spine_ui_for_forward()
            .and_then(SpineUiState::structured_content)
            .and_then(|content| content["agentSubtrees"].as_array().cloned())
            .is_some_and(|subtrees| subtrees.len() == 1)
    );
}

#[tokio::test]
async fn listener_failure_terminalizes_only_the_current_live_carrier() {
    let manager = ThreadStateManager::new();
    let thread_id = ThreadId::new();
    let connection_id = ConnectionId(1);
    manager
        .connection_initialized(connection_id, ConnectionCapabilities::default())
        .await;
    let thread_state = manager
        .try_ensure_connection_subscribed(thread_id, connection_id, false)
        .await
        .expect("connection should subscribe");
    {
        let mut state = thread_state.lock().await;
        state.listener_generation = 2;
        activate_spine_turn(&mut state, "turn-1", 1);
        state.record_spine_ui_spawn_progress(spawn_progress(ThreadId::new()));
        state.mark_spine_ui_mounted("turn-1", &[connection_id]);
    }
    let (outgoing_tx, mut outgoing_rx) = mpsc::channel(4);
    let outgoing = Arc::new(OutgoingMessageSender::new(
        outgoing_tx,
        codex_analytics::AnalyticsEventsClient::disabled(),
    ));

    assert!(
        !crate::spine_ui::listener::listener_failed_enabled_for_test(
            &manager,
            &thread_state,
            &outgoing,
            thread_id,
            1,
            "stale listener".to_string(),
        )
        .await
    );
    assert!(outgoing_rx.try_recv().is_err());
    assert!(
        crate::spine_ui::listener::listener_failed_enabled_for_test(
            &manager,
            &thread_state,
            &outgoing,
            thread_id,
            2,
            "event stream closed".to_string(),
        )
        .await
    );

    let OutgoingEnvelope::ToConnection {
        connection_id: sent_connection_id,
        message:
            OutgoingMessage::AppServerNotification(ServerNotification::ItemCompleted(notification)),
        ..
    } = outgoing_rx
        .recv()
        .await
        .expect("failed terminal notification")
    else {
        panic!("expected failed item/completed notification");
    };
    assert_eq!(sent_connection_id, connection_id);
    let ThreadItem::McpToolCall {
        status,
        result: Some(result),
        error: None,
        ..
    } = notification.item
    else {
        panic!("expected failed MCP carrier");
    };
    assert_eq!(status, McpToolCallStatus::Failed);
    assert_eq!(
        result.structured_content.as_ref().unwrap()["terminalReason"],
        "listener_error"
    );
    assert_eq!(
        result.structured_content.as_ref().unwrap()["spawnCalls"][0]["tasks"][0]["status"],
        "error"
    );
    assert!(result.content.iter().any(|content| {
        content["text"]
            .as_str()
            .is_some_and(|text| text.contains("event stream closed"))
    }));
}

#[tokio::test]
async fn turn_abort_terminalizes_the_live_carrier_as_failed() {
    let manager = ThreadStateManager::new();
    let thread_id = ThreadId::new();
    let connection_id = ConnectionId(1);
    manager
        .connection_initialized(connection_id, ConnectionCapabilities::default())
        .await;
    let thread_state = manager
        .try_ensure_connection_subscribed(thread_id, connection_id, false)
        .await
        .expect("connection should subscribe");
    {
        let mut state = thread_state.lock().await;
        state.listener_generation = 1;
        activate_spine_turn(&mut state, "turn-1", 1);
        state.record_spine_ui_spawn_progress(spawn_progress(ThreadId::new()));
        state.mark_spine_ui_mounted("turn-1", &[connection_id]);
    }
    let event = Event {
        id: "turn-1".to_string(),
        msg: turn_aborted("turn-1", TurnAbortReason::Interrupted),
    };
    let (outgoing_tx, mut outgoing_rx) = mpsc::channel(4);
    let outgoing = Arc::new(OutgoingMessageSender::new(
        outgoing_tx,
        codex_analytics::AnalyticsEventsClient::disabled(),
    ));
    let completion_token = crate::spine_ui::listener::before_bespoke_enabled_for_test(
        &outgoing,
        &[connection_id],
        &thread_state,
        thread_id,
        1,
        &event,
    )
    .await
    .expect("abort completion token");
    crate::spine_ui::listener::after_event_enabled_for_test(
        &manager,
        &thread_state,
        &outgoing,
        thread_id,
        1,
        &event,
        Some(completion_token),
    )
    .await;

    let OutgoingEnvelope::ToConnection {
        message:
            OutgoingMessage::AppServerNotification(ServerNotification::ItemCompleted(notification)),
        ..
    } = outgoing_rx
        .recv()
        .await
        .expect("aborted terminal notification")
    else {
        panic!("expected failed item/completed notification");
    };
    let ThreadItem::McpToolCall {
        status,
        result: Some(result),
        ..
    } = notification.item
    else {
        panic!("expected failed MCP carrier");
    };
    assert_eq!(status, McpToolCallStatus::Failed);
    assert_eq!(
        result.structured_content.as_ref().unwrap()["terminalReason"],
        "interrupted"
    );
    assert_eq!(
        result.structured_content.as_ref().unwrap()["spawnCalls"][0]["tasks"][0]["status"],
        "interrupted"
    );
}

#[tokio::test]
async fn rollback_invalidates_the_route_and_a_replacement_gets_a_new_generation() {
    let manager = ThreadStateManager::new();
    let parent_thread_id = ThreadId::new();
    let child_thread_id = ThreadId::new();
    let child_state = manager.thread_state(child_thread_id).await;
    {
        let mut state = child_state.lock().await;
        activate_spine_turn(&mut state, "child-turn", 3);
    }
    let (listener_tx, mut listener_rx) = mpsc::unbounded_channel();
    manager.register_listener_command_tx(parent_thread_id, listener_tx);
    let progress =
        register_test_route(&manager, parent_thread_id, "parent-turn", child_thread_id).await;
    let Command::ForwardSpineUiAgentState {
        generation: first_generation,
        state: Some(forwarded),
        ..
    } = recv_spine_ui_command(&mut listener_rx, "initial child state").await
    else {
        panic!("expected initial child state");
    };
    let parent_state = manager.thread_state(parent_thread_id).await;
    assert!(parent_state.lock().await.record_spine_ui_agent_state(
        child_thread_id,
        first_generation,
        forwarded,
    ));

    manager
        .clear_all_spine_ui_routes_for_thread(child_thread_id)
        .await;
    let Command::EmitSpineUiInvalidation { .. } =
        recv_spine_ui_command(&mut listener_rx, "rollback invalidation").await
    else {
        panic!("expected rollback invalidation");
    };
    assert!(
        !manager
            .spine_ui_route_is_current(
                child_thread_id,
                parent_thread_id,
                "parent-turn",
                0,
                first_generation,
            )
            .await
    );

    manager
        .register_spine_ui_spawn_progress(parent_thread_id, 0, "parent-turn", &progress)
        .await;
    let Command::ForwardSpineUiAgentState {
        generation: second_generation,
        ..
    } = recv_spine_ui_command(&mut listener_rx, "replacement child state").await
    else {
        panic!("expected replacement child state");
    };
    assert!(second_generation > first_generation);
}

#[tokio::test]
async fn parent_terminal_completes_without_waiting_for_a_child_ack() {
    let manager = ThreadStateManager::new();
    let parent_thread_id = ThreadId::new();
    let child_thread_id = ThreadId::new();
    let child_state = manager.thread_state(child_thread_id).await;
    {
        let mut state = child_state.lock().await;
        activate_spine_turn(&mut state, "child-turn", 4);
    }
    let (listener_tx, mut listener_rx) = mpsc::unbounded_channel();
    manager.register_listener_command_tx(parent_thread_id, listener_tx);
    register_test_route(&manager, parent_thread_id, "parent-turn", child_thread_id).await;
    let Command::ForwardSpineUiAgentState {
        generation: route_generation,
        ..
    } = recv_spine_ui_command(&mut listener_rx, "initial child state").await
    else {
        panic!("expected initial child state");
    };

    let parent_state = manager.thread_state(parent_thread_id).await;
    let completion_token = parent_state
        .lock()
        .await
        .begin_spine_ui_turn_completion("parent-turn")
        .expect("parent completion token");
    let (outgoing_tx, _outgoing_rx) = mpsc::channel(4);
    let outgoing = Arc::new(OutgoingMessageSender::new(
        outgoing_tx,
        codex_analytics::AnalyticsEventsClient::disabled(),
    ));
    let event = Event {
        id: "parent-turn".to_string(),
        msg: turn_complete("parent-turn"),
    };

    tokio::time::timeout(
        Duration::from_millis(100),
        crate::spine_ui::listener::after_event_enabled_for_test(
            &manager,
            &parent_state,
            &outgoing,
            parent_thread_id,
            0,
            &event,
            Some(completion_token),
        ),
    )
    .await
    .expect("parent completion must not wait for a child acknowledgement");

    assert!(
        parent_state
            .lock()
            .await
            .current_spine_ui_for_forward()
            .is_some_and(|state| state.completed_at_ms().is_some())
    );
    assert!(
        !manager
            .spine_ui_route_is_current(
                child_thread_id,
                parent_thread_id,
                "parent-turn",
                0,
                route_generation,
            )
            .await
    );
}

#[tokio::test]
async fn a_late_child_terminal_is_only_forwarded_to_a_future_parent_turn() {
    let manager = ThreadStateManager::new();
    let parent_thread_id = ThreadId::new();
    let child_thread_id = ThreadId::new();
    let child_state = manager.thread_state(child_thread_id).await;
    {
        let mut state = child_state.lock().await;
        activate_spine_turn(&mut state, "child-turn", 4);
    }
    manager
        .note_spine_ui_agent_turn_started(child_thread_id, 0, "child-turn")
        .await;
    let (listener_tx, mut listener_rx) = mpsc::unbounded_channel();
    manager.register_listener_command_tx(parent_thread_id, listener_tx);
    register_test_route(&manager, parent_thread_id, "parent-turn-1", child_thread_id).await;
    let Command::ForwardSpineUiAgentState { .. } =
        recv_spine_ui_command(&mut listener_rx, "initial child state").await
    else {
        panic!("expected initial child state");
    };
    manager
        .clear_spine_ui_parent_routes(parent_thread_id, "parent-turn-1", 0)
        .await;

    {
        let mut state = child_state.lock().await;
        finish_spine_turn(&mut state, "child-turn");
    }
    acknowledge_test_terminal(&manager, child_thread_id, 0, "child-turn").await;
    assert!(matches!(
        listener_rx.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));

    register_test_route(&manager, parent_thread_id, "parent-turn-2", child_thread_id).await;
    let Command::ForwardSpineUiAgentState {
        parent_turn_id,
        state: Some(state),
        ..
    } = recv_spine_ui_command(&mut listener_rx, "cached terminal state for the next route").await
    else {
        panic!("expected cached child terminal state");
    };
    assert_eq!(parent_turn_id, "parent-turn-2");
    assert!(state.completed_at_ms().is_some());
    assert_eq!(
        state
            .latest_snapshot()
            .expect("cached child terminal snapshot")
            .snapshot_seq,
        4
    );
}

#[tokio::test]
async fn rollback_clears_a_cached_child_terminal_before_reuse() {
    let manager = ThreadStateManager::new();
    let parent_thread_id = ThreadId::new();
    let child_thread_id = ThreadId::new();
    let child_state = manager.thread_state(child_thread_id).await;
    {
        let mut state = child_state.lock().await;
        activate_spine_turn(&mut state, "child-turn", 11);
        finish_spine_turn(&mut state, "child-turn");
    }
    acknowledge_test_terminal(&manager, child_thread_id, 0, "child-turn").await;

    child_state.lock().await.reset_spine_ui_after_rollback();
    manager
        .clear_all_spine_ui_routes_for_thread(child_thread_id)
        .await;
    let (listener_tx, mut listener_rx) = mpsc::unbounded_channel();
    manager.register_listener_command_tx(parent_thread_id, listener_tx);
    register_test_route(&manager, parent_thread_id, "parent-turn", child_thread_id).await;

    assert!(matches!(
        listener_rx.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
}
