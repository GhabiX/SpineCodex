use super::*;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::AgentStatus;
use codex_protocol::protocol::SpineSpawnTaskProgress;
use codex_protocol::protocol::SpineTreeNodeKind;
use codex_protocol::protocol::SpineTreeNodeSnapshot;
use codex_protocol::protocol::SpineTreeNodeStatus;
use codex_protocol::protocol::TurnCompleteEvent;
use codex_protocol::protocol::TurnStartedEvent;
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

fn turn_complete(turn_id: &str) -> EventMsg {
    EventMsg::TurnComplete(TurnCompleteEvent {
        turn_id: turn_id.to_string(),
        last_agent_message: None,
        completed_at: None,
        duration_ms: None,
        time_to_first_token_ms: None,
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
    state.track_current_turn_event(turn_id, &turn_started(turn_id));
    state.track_current_turn_event(turn_id, &spine_call());
    state.record_spine_ui_snapshot(tree_snapshot(sequence, "1.1", false));
}

fn finish_spine_turn(state: &mut ThreadState, turn_id: &str) {
    state.track_current_turn_event(turn_id, &turn_complete(turn_id));
    state.take_turn_summary();
}

async fn register_test_route(
    manager: &ThreadStateManager,
    parent_thread_id: ThreadId,
    parent_turn_id: &str,
    child_thread_id: ThreadId,
) -> SpineSpawnProgressEvent {
    let progress = spawn_progress(child_thread_id);
    let parent_state = manager.thread_state(parent_thread_id).await;
    {
        let mut state = parent_state.lock().await;
        activate_spine_turn(&mut state, parent_turn_id, 1);
        state.record_spine_ui_spawn_progress(progress.clone());
    }
    manager
        .register_spine_ui_spawn_progress(parent_thread_id, parent_turn_id, &progress)
        .await;
    progress
}

#[test]
fn each_turn_keeps_the_complete_tree() {
    let mut state = ThreadState::default();
    state.track_current_turn_event("turn-1", &turn_started("turn-1"));
    state.track_current_turn_event("turn-1", &spine_call());
    state.record_spine_ui_snapshot(tree_snapshot(1, "1.1", false));
    let first = state.take_turn_summary();
    assert_eq!(first.spine_ui.latest_snapshot().unwrap().nodes.len(), 2);

    state.track_current_turn_event("turn-2", &turn_started("turn-2"));
    state.track_current_turn_event("turn-2", &spine_call());
    assert!(state.turn_summary.spine_ui.latest_snapshot().is_none());
    state.record_spine_ui_snapshot(tree_snapshot(1, "1.2", false));

    let second = state
        .turn_summary
        .spine_ui
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
    state.track_current_turn_event("turn-1", &turn_started("turn-1"));
    state.track_current_turn_event("turn-1", &spine_call());
    state.record_spine_ui_snapshot(tree_snapshot(1, "1.1", false));
    state.take_turn_summary();

    state.track_current_turn_event("turn-2", &turn_started("turn-2"));
    state.track_current_turn_event("turn-2", &spine_call());
    state.record_spine_ui_snapshot(tree_snapshot(2, "1.1", true));

    let second = state
        .turn_summary
        .spine_ui
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

    state.track_current_turn_event("turn-1", &turn_started("turn-1"));
    state.track_current_turn_event("turn-1", &spine_call());
    state.record_spine_ui_snapshot(tree_snapshot(2, "1.2", false));
    let snapshot = state
        .turn_summary
        .spine_ui
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
        state.track_current_turn_event(&turn_id, &turn_started(&turn_id));
        state.track_current_turn_event(&turn_id, &spine_call());
        state.record_spine_ui_snapshot(growing_tree_snapshot(turn, turn));

        let node_count = state
            .turn_summary
            .spine_ui
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
    state.track_current_turn_event("turn-1", &turn_started("turn-1"));
    state.track_current_turn_event("turn-1", &spine_call());
    state.record_spine_ui_snapshot(tree_snapshot(1, "1.1", false));
    finish_spine_turn(&mut state, "turn-1");

    state.track_current_turn_event("turn-2", &turn_started("turn-2"));
    finish_spine_turn(&mut state, "turn-2");

    state.track_current_turn_event("turn-3", &turn_started("turn-3"));
    state.track_current_turn_event("turn-3", &spine_call());
    state.record_spine_ui_snapshot(tree_snapshot(1, "1.2", false));

    let snapshot = state
        .turn_summary
        .spine_ui
        .latest_snapshot()
        .expect("complete tree after an ordinary turn");
    assert_eq!(snapshot.nodes.len(), 3);
    assert_eq!(snapshot.active_node_id, "1.2");
}

#[test]
fn spawned_agent_subtrees_are_carried_into_the_next_complete_tree() {
    let mut state = ThreadState::default();
    let child_thread_id = ThreadId::new();

    state.track_current_turn_event("turn-1", &turn_started("turn-1"));
    state.track_current_turn_event("turn-1", &spine_call());
    state.record_spine_ui_snapshot(tree_snapshot(1, "1.1", false));
    assert!(state.record_spine_ui_spawn_progress(spawn_progress(child_thread_id)));

    let mut child_state = SpineUiState::default();
    child_state.record_snapshot(tree_snapshot(1, "1.1", false));
    assert!(state.record_spine_ui_agent_state(child_thread_id, 1, child_state));
    finish_spine_turn(&mut state, "turn-1");

    state.track_current_turn_event("turn-2", &turn_started("turn-2"));
    state.track_current_turn_event("turn-2", &spine_call());
    state.record_spine_ui_snapshot(tree_snapshot(1, "1.2", false));

    let content = state
        .turn_summary
        .spine_ui
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
    state.track_current_turn_event("turn-1", &turn_started("turn-1"));
    state.track_current_turn_event("turn-1", &spine_call());
    state.record_spine_ui_snapshot(tree_snapshot(1, "1.1", false));
    finish_spine_turn(&mut state, "turn-1");

    state.clear_listener();
    state.track_current_turn_event("turn-2", &turn_started("turn-2"));
    state.track_current_turn_event("turn-2", &spine_call());

    assert!(state.turn_summary.spine_ui.latest_snapshot().is_none());
}

#[test]
fn replacing_the_listener_drops_the_in_memory_complete_tree() {
    let mut state = ThreadState::default();
    state.track_current_turn_event("turn-1", &turn_started("turn-1"));
    state.track_current_turn_event("turn-1", &spine_call());
    state.record_spine_ui_snapshot(tree_snapshot(1, "1.1", false));
    finish_spine_turn(&mut state, "turn-1");

    let (first_cancel_tx, mut first_cancel_rx) = oneshot::channel();
    state.replace_listener_cancel_tx(first_cancel_tx);
    assert!(
        state
            .spine_ui_runtime
            .cumulative
            .latest_snapshot()
            .is_some()
    );

    let (replacement_cancel_tx, _replacement_cancel_rx) = oneshot::channel();
    state.replace_listener_cancel_tx(replacement_cancel_tx);
    assert_eq!(first_cancel_rx.try_recv(), Ok(()));

    state.track_current_turn_event("turn-2", &turn_started("turn-2"));
    state.track_current_turn_event("turn-2", &spine_call());
    assert!(state.turn_summary.spine_ui.latest_snapshot().is_none());
}

#[tokio::test]
async fn terminal_barrier_waits_for_the_matching_child_ack() {
    let manager = ThreadStateManager::new();
    let parent_thread_id = ThreadId::new();
    let child_thread_id = ThreadId::new();
    let child_state = manager.thread_state(child_thread_id).await;
    {
        let mut state = child_state.lock().await;
        activate_spine_turn(&mut state, "child-turn", 7);
    }
    register_test_route(&manager, parent_thread_id, "parent-turn", child_thread_id).await;

    let waiter_manager = manager.clone();
    let waiter = tokio::spawn(async move {
        waiter_manager
            .wait_for_spine_ui_terminal_children(
                parent_thread_id,
                "parent-turn",
                Duration::from_secs(2),
            )
            .await
    });
    tokio::task::yield_now().await;
    assert!(!waiter.is_finished());

    {
        let mut state = child_state.lock().await;
        finish_spine_turn(&mut state, "child-turn");
    }
    manager
        .acknowledge_spine_ui_agent_terminal(child_thread_id, 0, "child-turn")
        .await;

    let (states, timed_out) = waiter.await.expect("terminal barrier task");
    assert!(timed_out.is_empty());
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].0, child_thread_id);
    assert_eq!(
        states[0]
            .2
            .as_ref()
            .and_then(SpineUiState::latest_snapshot)
            .map(|snapshot| snapshot.snapshot_seq),
        Some(7)
    );
    assert!(
        states[0]
            .2
            .as_ref()
            .and_then(SpineUiState::latest_snapshot)
            .is_some_and(|snapshot| snapshot.nodes.is_empty())
    );
}

#[tokio::test]
async fn forwarded_agent_state_excludes_nodes_inherited_from_the_parent() {
    let manager = ThreadStateManager::new();
    let parent_thread_id = ThreadId::new();
    let child_thread_id = ThreadId::new();
    let child_state = manager.thread_state(child_thread_id).await;
    {
        let mut state = child_state.lock().await;
        state.track_current_turn_event("child-turn", &turn_started("child-turn"));
        state.track_current_turn_event("child-turn", &spine_call());
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

    let ThreadListenerCommand::ForwardSpineUiAgentState {
        state: Some(forwarded),
        ..
    } = listener_rx.recv().await.expect("initial child state")
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
    let ThreadListenerCommand::ForwardSpineUiAgentState {
        generation,
        state: Some(initial_state),
        ..
    } = listener_rx.recv().await.expect("initial child state")
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
        .register_spine_ui_spawn_progress(parent_thread_id, "parent-turn", &progress)
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
    let ThreadListenerCommand::ForwardSpineUiAgentState {
        state: Some(forwarded),
        ..
    } = listener_rx.recv().await.expect("updated child state")
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
    let ThreadListenerCommand::ForwardSpineUiAgentState {
        state: Some(coalesced),
        ..
    } = listener_rx
        .recv()
        .await
        .expect("coalesced latest child state")
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

#[tokio::test]
async fn terminal_barrier_observes_an_ack_before_route_registration() {
    let manager = ThreadStateManager::new();
    let parent_thread_id = ThreadId::new();
    let child_thread_id = ThreadId::new();
    let child_state = manager.thread_state(child_thread_id).await;
    {
        let mut state = child_state.lock().await;
        activate_spine_turn(&mut state, "child-turn", 9);
        finish_spine_turn(&mut state, "child-turn");
    }
    manager
        .acknowledge_spine_ui_agent_terminal(child_thread_id, 0, "child-turn")
        .await;
    register_test_route(&manager, parent_thread_id, "parent-turn", child_thread_id).await;

    let (states, timed_out) = manager
        .wait_for_spine_ui_terminal_children(parent_thread_id, "parent-turn", Duration::ZERO)
        .await;
    assert!(timed_out.is_empty());
    assert_eq!(
        states[0]
            .2
            .as_ref()
            .and_then(SpineUiState::latest_snapshot)
            .map(|snapshot| snapshot.snapshot_seq),
        Some(9)
    );
    assert!(
        states[0]
            .2
            .as_ref()
            .and_then(SpineUiState::latest_snapshot)
            .is_some_and(|snapshot| snapshot.nodes.is_empty())
    );

    manager
        .note_spine_ui_agent_turn_started(child_thread_id, 0)
        .await;
    let (_, timed_out) = manager
        .wait_for_spine_ui_terminal_children(parent_thread_id, "parent-turn", Duration::ZERO)
        .await;
    assert_eq!(
        timed_out
            .iter()
            .map(|(thread_id, _)| *thread_id)
            .collect::<Vec<_>>(),
        vec![child_thread_id]
    );
}

#[tokio::test]
#[expect(
    clippy::await_holding_invalid_type,
    reason = "the test deliberately holds both locks to reproduce the interleaving"
)]
async fn terminal_ack_settles_a_route_registered_while_reading_child_state() {
    let manager = ThreadStateManager::new();
    let parent_thread_id = ThreadId::new();
    let child_thread_id = ThreadId::new();
    let child_state = manager.thread_state(child_thread_id).await;
    {
        let mut state = child_state.lock().await;
        activate_spine_turn(&mut state, "child-turn", 10);
        finish_spine_turn(&mut state, "child-turn");
    }

    let child_guard = child_state.lock().await;
    let manager_guard = manager.state.lock().await;
    let ack_manager = manager.clone();
    let ack = tokio::spawn(async move {
        ack_manager
            .acknowledge_spine_ui_agent_terminal(child_thread_id, 0, "child-turn")
            .await;
    });
    tokio::task::yield_now().await;
    drop(manager_guard);
    drop(manager.state.lock().await);

    let route_manager = manager.clone();
    let route = tokio::spawn(async move {
        register_test_route(
            &route_manager,
            parent_thread_id,
            "parent-turn",
            child_thread_id,
        )
        .await;
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if manager
                .state
                .lock()
                .await
                .spine_ui_parent_by_child
                .contains_key(&child_thread_id)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("route registration");

    drop(child_guard);
    ack.await.expect("terminal ack task");
    route.await.expect("route registration task");

    let (states, timed_out) = manager
        .wait_for_spine_ui_terminal_children(parent_thread_id, "parent-turn", Duration::ZERO)
        .await;
    assert!(timed_out.is_empty());
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].0, child_thread_id);
    assert_eq!(
        states[0]
            .2
            .as_ref()
            .and_then(SpineUiState::latest_snapshot)
            .map(|snapshot| snapshot.snapshot_seq),
        Some(10)
    );
}

#[tokio::test]
async fn terminal_barrier_rejects_an_ack_from_a_stale_listener() {
    let manager = ThreadStateManager::new();
    let parent_thread_id = ThreadId::new();
    let child_thread_id = ThreadId::new();
    let child_state = manager.thread_state(child_thread_id).await;
    {
        let mut state = child_state.lock().await;
        state.listener_generation = 2;
        activate_spine_turn(&mut state, "child-turn", 4);
    }
    manager
        .note_spine_ui_listener_generation(child_thread_id, 2)
        .await;
    manager
        .note_spine_ui_listener_generation(child_thread_id, 1)
        .await;
    register_test_route(&manager, parent_thread_id, "parent-turn", child_thread_id).await;
    {
        let mut state = child_state.lock().await;
        finish_spine_turn(&mut state, "child-turn");
    }

    manager
        .acknowledge_spine_ui_agent_terminal(child_thread_id, 1, "child-turn")
        .await;
    let (_, timed_out) = manager
        .wait_for_spine_ui_terminal_children(parent_thread_id, "parent-turn", Duration::ZERO)
        .await;
    assert_eq!(
        timed_out
            .iter()
            .map(|(thread_id, _)| *thread_id)
            .collect::<Vec<_>>(),
        vec![child_thread_id]
    );

    manager
        .acknowledge_spine_ui_agent_terminal(child_thread_id, 2, "child-turn")
        .await;
    let (_, timed_out) = manager
        .wait_for_spine_ui_terminal_children(parent_thread_id, "parent-turn", Duration::ZERO)
        .await;
    assert!(timed_out.is_empty());
}

#[tokio::test]
async fn stale_turn_started_does_not_remove_routes_owned_by_a_new_listener() {
    let manager = ThreadStateManager::new();
    let parent_thread_id = ThreadId::new();
    let child_thread_id = ThreadId::new();
    manager
        .note_spine_ui_listener_generation(parent_thread_id, 2)
        .await;
    register_test_route(&manager, parent_thread_id, "parent-turn", child_thread_id).await;
    let (_, timed_out) = manager
        .wait_for_spine_ui_terminal_children(parent_thread_id, "parent-turn", Duration::ZERO)
        .await;
    let generation = timed_out[0].1;

    manager
        .note_spine_ui_agent_turn_started(parent_thread_id, 1)
        .await;

    assert!(
        manager
            .spine_ui_route_is_current(
                child_thread_id,
                parent_thread_id,
                "parent-turn",
                generation,
            )
            .await
    );
}

#[tokio::test]
async fn timed_out_child_can_refresh_the_latest_terminal_card() {
    let manager = ThreadStateManager::new();
    let parent_thread_id = ThreadId::new();
    let child_thread_id = ThreadId::new();
    let child_state = manager.thread_state(child_thread_id).await;
    {
        let mut state = child_state.lock().await;
        activate_spine_turn(&mut state, "child-turn", 5);
    }
    register_test_route(&manager, parent_thread_id, "parent-turn", child_thread_id).await;
    let (states, timed_out) = manager
        .wait_for_spine_ui_terminal_children(parent_thread_id, "parent-turn", Duration::ZERO)
        .await;
    let generation = states[0].1;
    assert_eq!(timed_out, vec![(child_thread_id, generation)]);
    assert!(
        states[0]
            .2
            .as_ref()
            .and_then(SpineUiState::latest_snapshot)
            .is_some_and(|snapshot| snapshot.nodes.is_empty())
    );

    let parent_state = manager.thread_state(parent_thread_id).await;
    {
        let mut state = parent_state.lock().await;
        assert!(state.record_spine_ui_agent_state(
            child_thread_id,
            generation,
            states[0].2.clone().expect("latest child state"),
        ));
        assert!(state.mark_spine_ui_agent_sync_timeout(child_thread_id, generation));
        finish_spine_turn(&mut state, "parent-turn");
    }
    for connection_id in [ConnectionId(7), ConnectionId(8)] {
        manager
            .connection_initialized(connection_id, ConnectionCapabilities::default())
            .await;
        manager
            .try_ensure_connection_subscribed(
                parent_thread_id,
                connection_id,
                /*experimental_raw_events*/ false,
            )
            .await
            .expect("connection should subscribe");
    }
    let connection_ids = manager.subscribed_connection_ids(parent_thread_id).await;
    parent_state
        .lock()
        .await
        .set_spine_ui_terminal_connection_ids("parent-turn", &connection_ids);
    assert!(
        manager
            .unsubscribe_connection_from_thread(parent_thread_id, ConnectionId(8))
            .await
    );
    let (listener_tx, mut listener_rx) = mpsc::unbounded_channel();
    manager.register_listener_command_tx(parent_thread_id, listener_tx);

    {
        let mut state = child_state.lock().await;
        finish_spine_turn(&mut state, "child-turn");
    }
    manager
        .acknowledge_spine_ui_agent_terminal(child_thread_id, 0, "child-turn")
        .await;
    let ThreadListenerCommand::ForwardSpineUiAgentState {
        generation: forwarded_generation,
        state: terminal_state,
        terminal,
        ..
    } = listener_rx.recv().await.expect("late terminal forward")
    else {
        panic!("expected late terminal forward");
    };
    assert_eq!(forwarded_generation, generation);
    assert!(terminal);
    assert!(
        terminal_state
            .as_ref()
            .and_then(SpineUiState::latest_snapshot)
            .is_some_and(|snapshot| snapshot.nodes.is_empty())
    );

    let refresh = parent_state
        .lock()
        .await
        .record_completed_spine_ui_agent_terminal(
            "parent-turn",
            child_thread_id,
            generation,
            terminal_state,
        )
        .expect("late terminal refresh");
    let content = refresh
        .state
        .structured_content()
        .expect("refreshed terminal card");
    assert_eq!(refresh.connection_ids, vec![ConnectionId(7)]);
    assert_ne!(
        content["spawnCalls"][0]["tasks"][0]["status"],
        serde_json::json!("error")
    );

    manager
        .complete_spine_ui_late_terminal(
            child_thread_id,
            parent_thread_id,
            "parent-turn",
            generation,
        )
        .await;
    assert!(
        !manager
            .spine_ui_route_is_current(
                child_thread_id,
                parent_thread_id,
                "parent-turn",
                generation,
            )
            .await
    );
}

#[tokio::test]
async fn parent_next_turn_drops_a_timed_out_route() {
    let manager = ThreadStateManager::new();
    let parent_thread_id = ThreadId::new();
    let child_thread_id = ThreadId::new();
    register_test_route(&manager, parent_thread_id, "parent-turn", child_thread_id).await;
    let (states, timed_out) = manager
        .wait_for_spine_ui_terminal_children(parent_thread_id, "parent-turn", Duration::ZERO)
        .await;
    let generation = states[0].1;
    assert_eq!(timed_out, vec![(child_thread_id, generation)]);

    manager
        .note_spine_ui_agent_turn_started(parent_thread_id, 0)
        .await;
    assert!(
        !manager
            .spine_ui_route_is_current(
                child_thread_id,
                parent_thread_id,
                "parent-turn",
                generation,
            )
            .await
    );
}

#[tokio::test]
async fn parent_route_is_retained_while_a_nested_agent_can_report_late_terminal() {
    let manager = ThreadStateManager::new();
    let ancestor_thread_id = ThreadId::new();
    let parent_thread_id = ThreadId::new();
    let nested_child_thread_id = ThreadId::new();
    let ancestor_state = manager.thread_state(ancestor_thread_id).await;
    let parent_state = manager.thread_state(parent_thread_id).await;
    {
        let mut state = parent_state.lock().await;
        activate_spine_turn(&mut state, "parent-turn", 2);
        state.record_spine_ui_spawn_progress(spawn_progress(nested_child_thread_id));
        assert!(state.mark_spine_ui_agent_sync_timeout(nested_child_thread_id, 1));
        finish_spine_turn(&mut state, "parent-turn");
    }
    manager
        .acknowledge_spine_ui_agent_terminal(parent_thread_id, 0, "parent-turn")
        .await;
    let (listener_tx, mut listener_rx) = mpsc::unbounded_channel();
    manager.register_listener_command_tx(ancestor_thread_id, listener_tx);

    register_test_route(
        &manager,
        ancestor_thread_id,
        "ancestor-turn",
        parent_thread_id,
    )
    .await;
    let (states, timed_out) = manager
        .wait_for_spine_ui_terminal_children(ancestor_thread_id, "ancestor-turn", Duration::ZERO)
        .await;
    assert!(timed_out.is_empty());
    let generation = states[0].1;
    {
        let mut state = ancestor_state.lock().await;
        assert!(state.record_spine_ui_agent_state(
            parent_thread_id,
            generation,
            states[0].2.clone().expect("parent terminal state"),
        ));
        finish_spine_turn(&mut state, "ancestor-turn");
    }
    let ThreadListenerCommand::ForwardSpineUiAgentState { .. } =
        listener_rx.recv().await.expect("initial parent state")
    else {
        panic!("expected initial parent state");
    };
    manager
        .clear_spine_ui_parent_routes(ancestor_thread_id, "ancestor-turn")
        .await;

    assert!(
        manager
            .spine_ui_route_is_current(
                parent_thread_id,
                ancestor_thread_id,
                "ancestor-turn",
                generation,
            )
            .await
    );

    let parent_refresh = parent_state
        .lock()
        .await
        .record_completed_spine_ui_agent_terminal(
            "parent-turn",
            nested_child_thread_id,
            1,
            Some(SpineUiState::default()),
        )
        .expect("nested late terminal refresh");
    assert!(!parent_refresh.state.has_agent_sync_timeout());
    manager
        .queue_spine_ui_agent_terminal_refresh(parent_thread_id)
        .await;
    let ThreadListenerCommand::ForwardSpineUiAgentState {
        state: Some(parent_terminal_state),
        terminal,
        ..
    } = listener_rx.recv().await.expect("ancestor terminal refresh")
    else {
        panic!("expected ancestor terminal refresh");
    };
    assert!(terminal);

    let ancestor_refresh = ancestor_state
        .lock()
        .await
        .record_completed_spine_ui_agent_terminal(
            "ancestor-turn",
            parent_thread_id,
            generation,
            Some(parent_terminal_state),
        )
        .expect("ancestor late terminal refresh");
    assert!(!ancestor_refresh.state.has_agent_sync_timeout());
}

#[tokio::test]
async fn listener_exit_clears_only_routes_owned_by_its_generation() {
    let manager = ThreadStateManager::new();
    let parent_thread_id = ThreadId::new();
    let child_thread_id = ThreadId::new();
    register_test_route(&manager, parent_thread_id, "parent-turn", child_thread_id).await;
    let (states, _) = manager
        .wait_for_spine_ui_terminal_children(parent_thread_id, "parent-turn", Duration::ZERO)
        .await;
    let route_generation = states[0].1;
    manager
        .note_spine_ui_listener_generation(child_thread_id, 2)
        .await;

    manager
        .clear_spine_ui_routes_for_listener_exit(child_thread_id, 1)
        .await;
    assert!(
        manager
            .spine_ui_route_is_current(
                child_thread_id,
                parent_thread_id,
                "parent-turn",
                route_generation,
            )
            .await
    );

    manager
        .clear_spine_ui_routes_for_listener_exit(child_thread_id, 2)
        .await;
    assert!(
        !manager
            .spine_ui_route_is_current(
                child_thread_id,
                parent_thread_id,
                "parent-turn",
                route_generation,
            )
            .await
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
    let ThreadListenerCommand::ForwardSpineUiAgentState {
        generation: first_generation,
        state: Some(forwarded),
        ..
    } = listener_rx.recv().await.expect("initial child state")
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
    let ThreadListenerCommand::EmitSpineUiInvalidation { .. } =
        listener_rx.recv().await.expect("rollback invalidation")
    else {
        panic!("expected rollback invalidation");
    };
    assert!(
        !manager
            .spine_ui_route_is_current(
                child_thread_id,
                parent_thread_id,
                "parent-turn",
                first_generation,
            )
            .await
    );

    manager
        .register_spine_ui_spawn_progress(parent_thread_id, "parent-turn", &progress)
        .await;
    let ThreadListenerCommand::ForwardSpineUiAgentState {
        generation: second_generation,
        ..
    } = listener_rx.recv().await.expect("replacement child state")
    else {
        panic!("expected replacement child state");
    };
    assert!(second_generation > first_generation);
}

#[tokio::test]
async fn rollback_clears_a_terminal_ack_before_the_child_is_reused() {
    let manager = ThreadStateManager::new();
    let parent_thread_id = ThreadId::new();
    let child_thread_id = ThreadId::new();
    let child_state = manager.thread_state(child_thread_id).await;
    {
        let mut state = child_state.lock().await;
        activate_spine_turn(&mut state, "child-turn", 11);
        finish_spine_turn(&mut state, "child-turn");
    }
    manager
        .acknowledge_spine_ui_agent_terminal(child_thread_id, 0, "child-turn")
        .await;

    child_state.lock().await.reset_spine_ui_after_rollback();
    manager
        .clear_all_spine_ui_routes_for_thread(child_thread_id)
        .await;
    register_test_route(&manager, parent_thread_id, "parent-turn", child_thread_id).await;

    let (states, timed_out) = manager
        .wait_for_spine_ui_terminal_children(parent_thread_id, "parent-turn", Duration::ZERO)
        .await;
    assert_eq!(timed_out, vec![(child_thread_id, states[0].1)]);
    assert!(states[0].2.is_none());
}
