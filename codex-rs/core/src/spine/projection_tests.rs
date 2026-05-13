use super::*;
use codex_protocol::config_types::ModeKind;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::protocol::ThreadRolledBackEvent;
use codex_protocol::protocol::TurnCompleteEvent;
use codex_protocol::protocol::TurnStartedEvent;

fn user_message(text: &str) -> RolloutItem {
    RolloutItem::ResponseItem(ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
    })
}

fn spine_call(call_id: &str, op: &str, summary: &str) -> RolloutItem {
    RolloutItem::ResponseItem(ResponseItem::FunctionCall {
        id: None,
        name: op.to_string(),
        namespace: Some(SPINE_NAMESPACE.to_string()),
        arguments: format!(r#"{{"summary":"{summary}"}}"#),
        call_id: call_id.to_string(),
    })
}

fn call_output(call_id: &str) -> RolloutItem {
    RolloutItem::ResponseItem(ResponseItem::FunctionCallOutput {
        call_id: call_id.to_string(),
        output: FunctionCallOutputPayload {
            body: FunctionCallOutputBody::Text("ok".to_string()),
            success: Some(true),
        },
    })
}

fn turn_started(turn_id: &str) -> RolloutItem {
    RolloutItem::EventMsg(EventMsg::TurnStarted(TurnStartedEvent {
        turn_id: turn_id.to_string(),
        started_at: None,
        model_context_window: None,
        collaboration_mode_kind: ModeKind::Default,
    }))
}

fn turn_complete(turn_id: &str) -> RolloutItem {
    RolloutItem::EventMsg(EventMsg::TurnComplete(TurnCompleteEvent {
        turn_id: turn_id.to_string(),
        last_agent_message: None,
        completed_at: None,
        duration_ms: None,
        time_to_first_token_ms: None,
    }))
}

#[test]
fn projects_committed_spine_transitions_from_rollout_prefix() {
    let projection = project_spine_state_from_rollout(&[
        user_message("start"),
        spine_call("open-1", SPINE_TOOL_OPEN, "scope"),
        call_output("open-1"),
        spine_call("next-1", SPINE_TOOL_NEXT, "done"),
        call_output("next-1"),
    ])
    .expect("project");

    assert_eq!(projection.response_item_count, 5);
    assert_eq!(projection.state.cursor().to_string(), "1.2");
    assert_eq!(
        projection
            .state
            .node(&NodeId::from_segments(vec![1, 1]))
            .expect("node")
            .summary
            .as_deref(),
        Some("done")
    );
}

#[test]
fn projection_applies_thread_rollback_markers() {
    let projection = project_spine_state_from_rollout(&[
        user_message("start"),
        spine_call("open-1", SPINE_TOOL_OPEN, "scope"),
        call_output("open-1"),
        user_message("drop this turn"),
        spine_call("next-1", SPINE_TOOL_NEXT, "rolled back"),
        call_output("next-1"),
        RolloutItem::EventMsg(EventMsg::ThreadRolledBack(ThreadRolledBackEvent {
            num_turns: 1,
        })),
    ])
    .expect("project");

    assert_eq!(projection.response_item_count, 3);
    assert_eq!(projection.state.cursor().to_string(), "1.1");
    assert!(
        projection
            .state
            .node(&NodeId::from_segments(vec![1, 2]))
            .is_none()
    );
}

#[test]
fn projection_keeps_only_surviving_turn_ids_after_rollback() {
    let projection = project_spine_state_from_rollout(&[
        turn_started("turn-1"),
        user_message("start"),
        spine_call("open-1", SPINE_TOOL_OPEN, "scope"),
        call_output("open-1"),
        turn_complete("turn-1"),
        turn_started("rolled-back-turn"),
        user_message("drop this turn"),
        spine_call("next-1", SPINE_TOOL_NEXT, "rolled back"),
        call_output("next-1"),
        turn_complete("rolled-back-turn"),
        RolloutItem::EventMsg(EventMsg::ThreadRolledBack(ThreadRolledBackEvent {
            num_turns: 1,
        })),
    ])
    .expect("project");

    assert!(projection.surviving_turn_ids.contains("turn-1"));
    assert!(!projection.surviving_turn_ids.contains("rolled-back-turn"));
}
