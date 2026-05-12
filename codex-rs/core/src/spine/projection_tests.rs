use super::*;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::protocol::ThreadRolledBackEvent;

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
