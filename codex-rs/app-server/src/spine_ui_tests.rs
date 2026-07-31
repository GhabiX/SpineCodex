use super::*;
use codex_app_server_protocol::McpToolCallStatus;
use codex_app_server_protocol::ThreadItem;
use codex_protocol::AgentPath;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::AgentStatus;
use codex_protocol::protocol::SpineSpawnOutcome;
use codex_protocol::protocol::SpineTreeNodeKind;
use codex_protocol::protocol::SpineTreeNodeSnapshot;
use codex_protocol::protocol::SpineTreeNodeStatus;
use pretty_assertions::assert_eq;
use serde_json::json;

fn snapshot(sequence: u64, active_node_id: &str) -> SpineTreeUpdateEvent {
    SpineTreeUpdateEvent {
        snapshot_seq: sequence,
        active_node_id: active_node_id.to_string(),
        settled_spawn_call_ids: Vec::new(),
        nodes: vec![
            node(
                "1",
                None,
                SpineTreeNodeKind::RootEpoch,
                SpineTreeNodeStatus::Opened,
                None,
            ),
            node(
                "1.1",
                Some("1"),
                SpineTreeNodeKind::Task,
                if active_node_id == "1.1" {
                    SpineTreeNodeStatus::Live
                } else {
                    SpineTreeNodeStatus::Closed
                },
                Some("Render the Spine tree"),
            ),
            node(
                "1.2",
                Some("1"),
                SpineTreeNodeKind::Task,
                if active_node_id == "1.2" {
                    SpineTreeNodeStatus::Live
                } else {
                    SpineTreeNodeStatus::Opened
                },
                Some("Verify the result"),
            ),
        ],
    }
}

fn node(
    node_id: &str,
    parent_id: Option<&str>,
    kind: SpineTreeNodeKind,
    status: SpineTreeNodeStatus,
    summary: Option<&str>,
) -> SpineTreeNodeSnapshot {
    SpineTreeNodeSnapshot {
        node_id: node_id.to_string(),
        parent_id: parent_id.map(str::to_string),
        kind,
        status,
        summary: summary.map(str::to_string),
        memory_summary: Some("not sent to the renderer".to_string()),
        spawn_outcome: None,
        start: 0,
        end: Some(10),
        context_pressure: None,
    }
}

fn spawn_progress(call_id: &str, status: AgentStatus) -> SpineSpawnProgressEvent {
    spawn_progress_for_thread(call_id, ThreadId::new(), status)
}

fn spawn_progress_for_thread(
    call_id: &str,
    thread_id: ThreadId,
    status: AgentStatus,
) -> SpineSpawnProgressEvent {
    SpineSpawnProgressEvent {
        call_id: call_id.to_string(),
        tasks: vec![SpineSpawnTaskProgress {
            ordinal: 0,
            summary: format!("Run {call_id}"),
            thread_id,
            agent_path: Some(
                AgentPath::try_from(format!("/root/{call_id}")).expect("valid agent path"),
            ),
            status,
        }],
    }
}

#[test]
fn only_tree_affecting_spine_calls_activate_the_ui() {
    for tool in ["open", "next", "close", "spawn"] {
        assert!(is_tree_tool_call(&function_call(tool, Some("spine"))));
        assert!(is_tree_tool_call(&function_call(
            &format!("spine.{tool}"),
            None
        )));
    }
    assert!(!is_tree_tool_call(&function_call("trim", Some("spine"))));
    assert!(!is_tree_tool_call(&function_call("open", Some("other"))));
    assert!(!is_tree_tool_call(&function_call("shell", None)));
}

fn function_call(name: &str, namespace: Option<&str>) -> ResponseItem {
    ResponseItem::FunctionCall {
        id: None,
        name: name.to_string(),
        namespace: namespace.map(str::to_string),
        arguments: "{}".to_string(),
        call_id: format!("call-{name}"),
        internal_chat_message_metadata_passthrough: None,
    }
}

#[test]
fn resource_is_scoped_and_self_contained() {
    let response = read_resource(SERVER_NAME, RESOURCE_URI).expect("Spine UI resource");
    assert_eq!(response.contents.len(), 1);
    assert!(read_resource("other", RESOURCE_URI).is_none());
    assert!(read_resource(SERVER_NAME, "ui://spine/other.html").is_none());
    assert!(RESOURCE_HTML.contains("default-src 'none'"));
    assert!(RESOURCE_HTML.contains("ResizeObserver"));
    assert!(RESOURCE_HTML.contains("function validTreePayload(value)"));
    assert!(RESOURCE_HTML.contains("spawnCalls.flatMap"));
    assert!(!RESOURCE_HTML.contains("<script src="));
    assert!(!RESOURCE_HTML.contains("<link rel=\"stylesheet\""));
    assert!(!RESOURCE_HTML.contains("https://"));

    let status = server_status(true);
    let tool = status.tools.get("spine_tree").expect("Spine Tree tool");
    assert_eq!(tool.title.as_deref(), Some("Spine Tree"));
    assert_eq!(status.name, SERVER_NAME);
    assert_eq!(status.resources[0].uri, RESOURCE_URI);
    assert_eq!(
        tool.meta.as_ref().expect("tool metadata")["ui"]["resourceUri"],
        RESOURCE_URI
    );
}

#[test]
fn render_payload_contains_only_fields_used_by_the_card() {
    let mut state = SpineUiState::default();
    state.record_snapshot(snapshot(5, "1.1"));
    state.record_spawn_progress(spawn_progress("child", AgentStatus::Running));

    let response = tool_call_response("thread-1", Some(&state));
    let content = response.structured_content.expect("structured content");

    assert_eq!(response.is_error, Some(false));
    assert_eq!(content["schemaVersion"], 1);
    assert_eq!(content["snapshot"]["activeNodeId"], "1.1");
    assert!(content["snapshot"].get("snapshotSeq").is_none());
    assert!(
        content["snapshot"]["nodes"][0]
            .get("memorySummary")
            .is_none()
    );
    assert!(content["snapshot"]["nodes"][0].get("end").is_none());
    assert!(
        content["snapshot"]["nodes"][0]
            .get("contextPressure")
            .is_none()
    );
    assert!(
        content["spawnCalls"][0]["tasks"][0]
            .get("agentPath")
            .is_none()
    );
    assert!(content.get("agentGenerations").is_none());
    assert!(content.get("invalidatedAgentGenerations").is_none());
    assert!(content.get("agentSyncTimeoutGenerations").is_none());
    assert!(content.get("suppressedNodeIds").is_none());
}

#[test]
fn spawn_calls_keep_creation_order_and_original_parent() {
    let mut state = SpineUiState::default();
    state.record_snapshot(snapshot(1, "1.1"));
    state.record_spawn_progress(spawn_progress("first", AgentStatus::Running));
    state.record_snapshot(snapshot(2, "1.2"));
    state.record_spawn_progress(spawn_progress("second", AgentStatus::Running));
    state.record_spawn_progress(spawn_progress("first", AgentStatus::Completed(None)));

    let content = state.structured_content().expect("structured content");
    assert_eq!(content["spawnCalls"][0]["callId"], "first");
    assert_eq!(content["spawnCalls"][0]["parentNodeId"], "1.1");
    assert_eq!(
        content["spawnCalls"][0]["tasks"][0]["status"],
        json!("completed")
    );
    assert_eq!(content["spawnCalls"][1]["callId"], "second");
    assert_eq!(content["spawnCalls"][1]["parentNodeId"], "1.2");
}

#[test]
fn changed_snapshot_at_the_same_boundary_advances_the_ui_revision() {
    let mut state = SpineUiState::default();
    state.record_snapshot(snapshot(7, "1.1"));
    let first_revision = state.revision;

    state.record_snapshot(snapshot(7, "1.2"));
    assert!(state.revision > first_revision);
    let second_revision = state.revision;
    state.record_snapshot(snapshot(7, "1.2"));
    assert_eq!(state.revision, second_revision);
}

#[test]
fn replacement_generation_cannot_be_removed_by_an_older_terminal() {
    let child_thread_id = ThreadId::new();
    let mut state = SpineUiState::default();
    state.record_snapshot(snapshot(1, "1.1"));
    state.record_spawn_progress(spawn_progress_for_thread(
        "child",
        child_thread_id,
        AgentStatus::Running,
    ));

    let mut first = SpineUiState::default();
    first.record_snapshot(snapshot(8, "1.2"));
    assert!(state.record_agent_state(child_thread_id, 1, first));
    let mut replacement = SpineUiState::default();
    replacement.record_snapshot(snapshot(1, "1.1"));
    assert!(state.record_agent_state(child_thread_id, 2, replacement));
    assert!(!state.remove_agent_state(child_thread_id, 1));
    assert!(state.remove_agent_state(child_thread_id, 2));
}

#[test]
fn agent_sync_timeout_is_visible_until_the_matching_terminal_state_arrives() {
    let child_thread_id = ThreadId::new();
    let mut state = SpineUiState::default();
    state.record_snapshot(snapshot(1, "1.1"));
    state.record_spawn_progress(spawn_progress_for_thread(
        "child",
        child_thread_id,
        AgentStatus::Running,
    ));

    assert!(state.mark_agent_sync_timeout(child_thread_id, 7));
    assert_eq!(
        state.structured_content().expect("timed out content")["spawnCalls"][0]["tasks"][0]["status"],
        json!("error")
    );
    assert!(!state.clear_agent_sync_timeout(child_thread_id, 6));
    assert!(state.clear_agent_sync_timeout(child_thread_id, 7));
}

#[test]
fn render_payload_omits_agent_terminal_messages() {
    let mut state = SpineUiState::default();
    state.record_snapshot(snapshot(1, "1.1"));
    state.record_spawn_progress(spawn_progress(
        "failed",
        AgentStatus::Errored("sensitive diagnostic".to_string()),
    ));
    state.record_spawn_progress(spawn_progress(
        "completed",
        AgentStatus::Completed(Some("sensitive final answer".to_string())),
    ));

    let content = state.structured_content().expect("structured content");
    assert_eq!(content["spawnCalls"][0]["tasks"][0]["status"], "error");
    assert_eq!(content["spawnCalls"][1]["tasks"][0]["status"], "completed");
    let serialized = content.to_string();
    assert!(!serialized.contains("sensitive diagnostic"));
    assert!(!serialized.contains("sensitive final answer"));
}

#[test]
fn settled_spawn_call_links_its_result_node_without_duplicate_payload_metadata() {
    let mut state = SpineUiState::default();
    state.record_snapshot(snapshot(1, "1.1"));
    state.record_spawn_progress(spawn_progress("settled", AgentStatus::Running));

    let mut committed = snapshot(2, "1.1");
    committed.settled_spawn_call_ids = vec!["settled".to_string()];
    let mut result_node = node(
        "1.1.1",
        Some("1.1"),
        SpineTreeNodeKind::Task,
        SpineTreeNodeStatus::Closed,
        Some("Run settled"),
    );
    result_node.spawn_outcome = Some(SpineSpawnOutcome::Completed);
    committed.nodes.push(result_node);
    state.record_snapshot(committed);

    let content = state.structured_content().expect("structured content");
    assert_eq!(
        content["spawnCalls"][0]["tasks"][0]["resultNodeId"],
        "1.1.1"
    );
    assert_eq!(content["snapshot"]["nodes"][3]["nodeId"], "1.1.1");
    assert!(content.get("suppressedNodeIds").is_none());
}

#[test]
fn agent_subtree_is_nested_under_its_matching_agent() {
    let child_thread_id = ThreadId::new();
    let mut state = SpineUiState::default();
    state.record_snapshot(snapshot(1, "1.1"));
    state.record_spawn_progress(spawn_progress_for_thread(
        "child",
        child_thread_id,
        AgentStatus::Running,
    ));
    let mut child_state = SpineUiState::default();
    child_state.record_snapshot(snapshot(4, "1.2"));
    state.record_agent_state(child_thread_id, 1, child_state);

    let content = state.structured_content().expect("structured content");
    assert_eq!(
        content["agentSubtrees"][0]["threadId"],
        child_thread_id.to_string()
    );
    assert_eq!(
        content["agentSubtrees"][0]["snapshot"]["activeNodeId"],
        "1.2"
    );
}

#[test]
fn agent_with_a_parent_outside_the_current_snapshot_is_rendered_at_the_root() {
    let child_thread_id = ThreadId::new();
    let mut state = SpineUiState::default();
    state.record_snapshot(snapshot(1, "1.1"));
    state.record_spawn_progress(spawn_progress_for_thread(
        "child",
        child_thread_id,
        AgentStatus::Running,
    ));

    let mut child_state = SpineUiState::default();
    child_state.record_snapshot(snapshot(1, "1.2"));
    state.record_agent_state(child_thread_id, 1, child_state);

    let snapshot_without_parent = SpineTreeUpdateEvent {
        snapshot_seq: 2,
        active_node_id: "2.1".to_string(),
        settled_spawn_call_ids: Vec::new(),
        nodes: vec![node(
            "2.1",
            None,
            SpineTreeNodeKind::Task,
            SpineTreeNodeStatus::Live,
            Some("Current turn"),
        )],
    };
    state.record_snapshot(snapshot_without_parent);

    let content = state.structured_content().expect("structured content");
    assert_eq!(content["spawnCalls"][0]["callId"], "child");
    assert!(content["spawnCalls"][0].get("parentNodeId").is_none());
    assert_eq!(
        content["agentSubtrees"][0]["threadId"],
        child_thread_id.to_string()
    );
}

#[test]
fn parent_filter_removes_inherited_nodes_and_reanchors_nested_agents() {
    let grandchild_thread_id = ThreadId::new();
    let mut child_state = SpineUiState::default();
    child_state.record_snapshot(snapshot(4, "1.2"));
    child_state.record_spawn_progress(spawn_progress_for_thread(
        "nested",
        grandchild_thread_id,
        AgentStatus::Running,
    ));
    let baseline_node_ids = child_state
        .latest_snapshot()
        .expect("child snapshot")
        .nodes
        .iter()
        .map(|node| node.node_id.clone())
        .collect();

    let filtered = child_state.filtered_for_parent(&baseline_node_ids);
    let content = filtered.structured_content().expect("filtered content");

    assert_eq!(content["snapshot"]["nodes"], json!([]));
    assert!(content["spawnCalls"][0].get("parentNodeId").is_none());
    assert_eq!(
        content["spawnCalls"][0]["tasks"][0]["threadId"],
        grandchild_thread_id.to_string()
    );
}

#[test]
fn live_item_uses_one_stable_id_per_turn() {
    let mut state = SpineUiState::default();
    state.record_snapshot(snapshot(7, "1.1"));
    let started =
        snapshot_started_notification("thread-1", "turn-1", &state).expect("started notification");
    state.record_snapshot(snapshot(8, "1.2"));
    let started_refresh = snapshot_started_notification("thread-1", "turn-1", &state)
        .expect("started refresh notification");
    state.mark_completed();
    let completed = snapshot_completed_notification("thread-1", "turn-1", &state)
        .expect("completed notification");
    let completed_refresh = snapshot_completed_notification("thread-1", "turn-1", &state)
        .expect("completed refresh notification");
    let other_turn = snapshot_completed_notification("thread-1", "turn-2", &state)
        .expect("second completed notification");

    let ThreadItem::McpToolCall {
        id: started_id,
        status: started_status,
        result: Some(started_result),
        ..
    } = started.item
    else {
        panic!("expected MCP tool call item");
    };
    let ThreadItem::McpToolCall {
        id: completed_id,
        status: completed_status,
        result: Some(completed_result),
        ..
    } = completed.item
    else {
        panic!("expected MCP tool call item");
    };
    let ThreadItem::McpToolCall {
        result: Some(other_result),
        ..
    } = other_turn.item
    else {
        panic!("expected second MCP tool call result");
    };

    assert_eq!(started_id, completed_id);
    assert_eq!(started.started_at_ms, started_refresh.started_at_ms);
    assert_eq!(completed.completed_at_ms, completed_refresh.completed_at_ms);
    assert_eq!(started_status, McpToolCallStatus::InProgress);
    assert_eq!(completed_status, McpToolCallStatus::Completed);
    assert_eq!(started_result.meta, completed_result.meta);
    assert_ne!(started_result.meta, other_result.meta);
}
