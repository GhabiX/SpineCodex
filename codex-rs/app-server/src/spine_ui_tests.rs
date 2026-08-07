use super::*;
use codex_app_server_protocol::McpToolCallStatus;
use codex_app_server_protocol::ThreadItem;
use codex_protocol::AgentPath;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::AgentStatus;
use codex_protocol::protocol::SpineSpawnOutcome;
use codex_protocol::protocol::SpineTreeNodeKind;
use codex_protocol::protocol::SpineTreeNodeSnapshot;
use codex_protocol::protocol::SpineTreeNodeStatus;
use codex_protocol::protocol::TurnAbortReason;
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
        assert!(is_tree_affecting_item(&function_call(tool, Some("spine"))));
        assert!(is_tree_affecting_item(&function_call(
            &format!("spine.{tool}"),
            None
        )));
    }
    assert!(!is_tree_affecting_item(&function_call(
        "trim",
        Some("spine")
    )));
    assert!(!is_tree_affecting_item(&function_call(
        "open",
        Some("other")
    )));
    assert!(!is_tree_affecting_item(&function_call("shell", None)));
}

#[test]
fn code_mode_spine_carriers_activate_only_for_tree_controls() {
    for tool in ["open", "next", "close", "spawn"] {
        assert!(is_tree_affecting_item(&code_mode_carrier(
            Some(CODE_MODE_SPINE_CARRIER_MARKER),
            CODE_MODE_SPINE_CARRIER_MARKER,
            tool,
        )));
    }
    assert!(!is_tree_affecting_item(&code_mode_carrier(
        Some(CODE_MODE_SPINE_CARRIER_MARKER),
        CODE_MODE_SPINE_CARRIER_MARKER,
        "trim",
    )));
    assert!(!is_tree_affecting_item(&code_mode_carrier(
        None,
        CODE_MODE_SPINE_CARRIER_MARKER,
        "close",
    )));
    assert!(!is_tree_affecting_item(&code_mode_carrier(
        Some(CODE_MODE_SPINE_CARRIER_MARKER),
        "spine.code_mode.output.v0",
        "close",
    )));

    let malformed = ResponseItem::CustomToolCallOutput {
        id: None,
        call_id: "carrier-malformed".to_string(),
        name: Some(CODE_MODE_SPINE_CARRIER_MARKER.to_string()),
        output: FunctionCallOutputPayload::from_text("not json".to_string()),
        internal_chat_message_metadata_passthrough: None,
    };
    assert!(!is_tree_affecting_item(&malformed));

    let non_text = ResponseItem::CustomToolCallOutput {
        id: None,
        call_id: "carrier-content-items".to_string(),
        name: Some(CODE_MODE_SPINE_CARRIER_MARKER.to_string()),
        output: FunctionCallOutputPayload::from_content_items(vec![
            FunctionCallOutputContentItem::InputText {
                text: "not a carrier".to_string(),
            },
        ]),
        internal_chat_message_metadata_passthrough: None,
    };
    assert!(!is_tree_affecting_item(&non_text));
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

fn code_mode_carrier(output_name: Option<&str>, schema: &str, nested_tool: &str) -> ResponseItem {
    ResponseItem::CustomToolCallOutput {
        id: None,
        call_id: "carrier-call".to_string(),
        name: output_name.map(str::to_string),
        output: FunctionCallOutputPayload::from_text(
            json!({
                "schema": schema,
                "nested_spine_calls": [{"name": nested_tool}],
            })
            .to_string(),
        ),
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
fn resource_keeps_settled_agents_at_their_result_node_order() {
    assert!(RESOURCE_HTML.contains("const agentsByResultNodeId = new Map()"));
    assert!(RESOURCE_HTML.contains("if (linkedAgent) return renderAgent(linkedAgent"));
    assert!(RESOURCE_HTML.contains(
        "if (task.resultNodeId && context.agentsByResultNodeId.has(task.resultNodeId)) return"
    ));
    assert!(!RESOURCE_HTML.contains("suppressedNodeIds"));
}

#[test]
fn resource_applies_terminal_outcome_to_the_top_level_tree() {
    assert!(RESOURCE_HTML.contains(
        "const terminalState = [\"completed\", \"aborted\", \"failed\"].includes(payload.terminalOutcome)"
    ));
    assert!(RESOURCE_HTML.contains("const context = buildContext(payload, terminalState)"));
    assert!(!RESOURCE_HTML.contains("const context = buildContext(payload);"));
}

#[test]
fn resource_renders_ambiguous_aborted_results_as_interrupted() {
    assert!(RESOURCE_HTML.contains("if (outcome === \"aborted\") return \"interrupted\";"));
    assert!(!RESOURCE_HTML.contains("if (outcome === \"aborted\") return \"shutdown\";"));
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
fn settled_spawn_call_on_root_epoch_is_rendered_as_a_top_level_agent() {
    let mut state = SpineUiState::default();
    state.record_snapshot(snapshot(1, "1"));
    state.record_spawn_progress(spawn_progress("root_child", AgentStatus::Running));

    let mut committed = snapshot(2, "1");
    committed.settled_spawn_call_ids = vec!["root_child".to_string()];
    let mut result_node = node(
        "1.3",
        Some("1"),
        SpineTreeNodeKind::Task,
        SpineTreeNodeStatus::Closed,
        Some("Run root_child"),
    );
    result_node.spawn_outcome = Some(SpineSpawnOutcome::Completed);
    committed.nodes.push(result_node);
    state.record_snapshot(committed);

    let content = state.structured_content().expect("structured content");
    assert!(content["spawnCalls"][0]["parentNodeId"].is_null());
    assert_eq!(content["spawnCalls"][0]["tasks"][0]["resultNodeId"], "1.3");
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
fn unchanged_spawn_progress_does_not_advance_the_ui_revision() {
    let child_thread_id = ThreadId::new();
    let mut state = SpineUiState::default();
    state.record_snapshot(snapshot(1, "1.1"));
    let running = spawn_progress_for_thread("child", child_thread_id, AgentStatus::Running);

    assert!(state.record_spawn_progress(running.clone()));
    let running_revision = state.revision;
    assert!(!state.record_spawn_progress(running.clone()));
    assert_eq!(state.revision, running_revision);

    assert!(state.record_spawn_progress(spawn_progress_for_thread(
        "child",
        child_thread_id,
        AgentStatus::Completed(None),
    )));
    let terminal_revision = state.revision;
    assert!(!state.record_spawn_progress(running));
    assert_eq!(state.revision, terminal_revision);
}

#[test]
fn specific_terminal_status_is_not_overwritten_by_the_interrupted_fallback() {
    let child_thread_id = ThreadId::new();
    let mut state = SpineUiState::default();
    state.record_snapshot(snapshot(1, "1.1"));

    assert!(state.record_spawn_progress(spawn_progress_for_thread(
        "child",
        child_thread_id,
        AgentStatus::Shutdown,
    )));
    assert!(!state.record_spawn_progress(spawn_progress_for_thread(
        "child",
        child_thread_id,
        AgentStatus::Interrupted,
    )));

    let content = state.structured_content().expect("structured content");
    assert_eq!(content["spawnCalls"][0]["tasks"][0]["status"], "shutdown");
}

#[test]
fn aborted_result_uses_interrupted_fallback_without_overwriting_shutdown() {
    let mut state = SpineUiState::default();
    state.record_snapshot(snapshot(1, "1.1"));
    state.record_spawn_progress(spawn_progress("ambiguous_abort", AgentStatus::Running));
    state.record_spawn_progress(spawn_progress("true_shutdown", AgentStatus::Shutdown));

    let mut committed = snapshot(2, "1.1");
    committed.settled_spawn_call_ids =
        vec!["ambiguous_abort".to_string(), "true_shutdown".to_string()];
    for (node_id, summary) in [
        ("1.1.1", "Run ambiguous_abort"),
        ("1.1.2", "Run true_shutdown"),
    ] {
        let mut result_node = node(
            node_id,
            Some("1.1"),
            SpineTreeNodeKind::Task,
            SpineTreeNodeStatus::Closed,
            Some(summary),
        );
        result_node.spawn_outcome = Some(SpineSpawnOutcome::Aborted);
        committed.nodes.push(result_node);
    }
    assert!(state.record_snapshot(committed));

    let content = state.structured_content().expect("structured content");
    assert_eq!(
        content["spawnCalls"][0]["tasks"][0]["status"],
        "interrupted"
    );
    assert_eq!(content["spawnCalls"][1]["tasks"][0]["status"], "shutdown");
}

#[test]
fn terminalizing_incomplete_agents_is_recursive_and_preserves_existing_terminals() {
    let child_thread_id = ThreadId::new();
    let completed_thread_id = ThreadId::new();
    let mut state = SpineUiState::default();
    state.record_snapshot(snapshot(1, "1.1"));
    state.record_spawn_progress(spawn_progress_for_thread(
        "child",
        child_thread_id,
        AgentStatus::Running,
    ));
    state.record_spawn_progress(spawn_progress_for_thread(
        "completed",
        completed_thread_id,
        AgentStatus::Completed(None),
    ));

    let mut child_state = SpineUiState::default();
    child_state.record_snapshot(snapshot(1, "1.1"));
    child_state.record_spawn_progress(spawn_progress("grandchild", AgentStatus::PendingInit));
    assert!(state.record_agent_state(child_thread_id, 1, child_state));

    assert!(state.terminalize_incomplete_agents(AgentStatus::Interrupted));
    let content = state.structured_content().expect("terminal content");
    assert_eq!(
        content["spawnCalls"][0]["tasks"][0]["status"],
        "interrupted"
    );
    assert_eq!(content["spawnCalls"][1]["tasks"][0]["status"], "completed");
    assert_eq!(
        content["agentSubtrees"][0]["spawnCalls"][0]["tasks"][0]["status"],
        "interrupted"
    );
    assert!(!state.terminalize_incomplete_agents(AgentStatus::Interrupted));
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
    assert!(content["spawnCalls"][0]["parentNodeId"].is_null());
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
    assert!(content["spawnCalls"][0]["parentNodeId"].is_null());
    assert_eq!(
        content["spawnCalls"][0]["tasks"][0]["threadId"],
        grandchild_thread_id.to_string()
    );
}

#[test]
fn reserved_live_carrier_upserts_one_stable_id_per_turn() {
    let mut state = SpineUiState::default();
    state.record_snapshot(snapshot(7, "1.1"));
    let first_upsert =
        snapshot_upsert_notification("thread-1", "turn-1", &state).expect("first upsert");
    state.record_snapshot(snapshot(8, "1.2"));
    let refreshed_upsert =
        snapshot_upsert_notification("thread-1", "turn-1", &state).expect("refreshed upsert");
    state.mark_completed();
    let completed = snapshot_terminal_notification(
        "thread-1",
        "turn-1",
        &state,
        &SpineUiTerminalOutcome::Completed,
    )
    .expect("completed notification");
    let completed_refresh = snapshot_terminal_notification(
        "thread-1",
        "turn-1",
        &state,
        &SpineUiTerminalOutcome::Completed,
    )
    .expect("completed refresh notification");
    let other_turn = snapshot_terminal_notification(
        "thread-1",
        "turn-2",
        &state,
        &SpineUiTerminalOutcome::Completed,
    )
    .expect("second completed notification");

    let ThreadItem::McpToolCall {
        id: started_id,
        status: started_status,
        result: Some(started_result),
        ..
    } = first_upsert.item
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
    assert_eq!(first_upsert.started_at_ms, refreshed_upsert.started_at_ms);
    assert_eq!(completed.completed_at_ms, completed_refresh.completed_at_ms);
    assert_eq!(started_status, McpToolCallStatus::InProgress);
    assert_eq!(completed_status, McpToolCallStatus::Completed);
    assert_eq!(
        completed_result.structured_content.as_ref().unwrap()["terminalOutcome"],
        "completed"
    );
    assert_eq!(started_result.meta, completed_result.meta);
    assert_ne!(started_result.meta, other_result.meta);
}

#[test]
fn terminal_carrier_reports_abort_and_listener_failure_as_failed() {
    let mut state = SpineUiState::default();
    state.record_snapshot(snapshot(9, "1.2"));
    state.mark_completed();

    for (outcome, expected_reason) in [
        (
            SpineUiTerminalOutcome::Aborted(TurnAbortReason::Interrupted),
            "interrupted",
        ),
        (
            SpineUiTerminalOutcome::Aborted(TurnAbortReason::Replaced),
            "replaced",
        ),
        (
            SpineUiTerminalOutcome::Aborted(TurnAbortReason::ReviewEnded),
            "review_ended",
        ),
        (
            SpineUiTerminalOutcome::Aborted(TurnAbortReason::BudgetLimited),
            "budget_limited",
        ),
        (
            SpineUiTerminalOutcome::ListenerFailed("event stream closed".to_string()),
            "listener_error",
        ),
    ] {
        let notification = snapshot_terminal_notification("thread-1", "turn-1", &state, &outcome)
            .expect("failed terminal notification");
        let ThreadItem::McpToolCall {
            status,
            result: Some(result),
            error: None,
            ..
        } = notification.item
        else {
            panic!("expected failed MCP tool call item");
        };

        assert_eq!(status, McpToolCallStatus::Failed);
        assert_eq!(
            result.structured_content.as_ref().unwrap()["terminalReason"],
            expected_reason
        );
        assert!(!result.content.is_empty());
    }
}
