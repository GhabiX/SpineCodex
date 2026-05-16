use super::*;
use crate::spine::state::NodeStatus;
use codex_protocol::config_types::ModeKind;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::protocol::CompactedItem;
use codex_protocol::protocol::ThreadRolledBackEvent;
use codex_protocol::protocol::TurnCompleteEvent;
use codex_protocol::protocol::TurnStartedEvent;

fn id(segments: &[u32]) -> NodeId {
    NodeId::from_segments(segments.to_vec())
}

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
        arguments: if op == SPINE_TOOL_OPEN {
            "{}".to_string()
        } else {
            format!(r#"{{"summary":"{summary}"}}"#)
        },
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

fn compacted(message: &str) -> RolloutItem {
    RolloutItem::Compacted(CompactedItem {
        message: message.to_string(),
        replacement_history: Some(Vec::new()),
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
    assert_eq!(projection.state.cursor().to_string(), "1.1.2");
    assert_eq!(
        projection
            .state
            .node(&NodeId::from_segments(vec![1, 1, 1]))
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
    assert_eq!(projection.state.cursor().to_string(), "1.1.1");
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

#[test]
fn projection_keeps_only_surviving_compact_hashes_after_rollback() {
    let surviving_message = "Spine compacted 1.1 [1, 4)";
    let rolled_back_message = "Spine compacted 1.1 [1, 6)";
    let projection = project_spine_state_from_rollout(&[
        turn_started("turn-1"),
        user_message("start"),
        compacted(surviving_message),
        turn_complete("turn-1"),
        turn_started("rolled-back-turn"),
        user_message("redo this turn"),
        compacted(rolled_back_message),
        turn_complete("rolled-back-turn"),
        RolloutItem::EventMsg(EventMsg::ThreadRolledBack(ThreadRolledBackEvent {
            num_turns: 1,
        })),
    ])
    .expect("project");

    assert!(
        projection
            .surviving_compact_hashes
            .contains(&compact_message_hash(surviving_message))
    );
    assert!(
        !projection
            .surviving_compact_hashes
            .contains(&compact_message_hash(rolled_back_message))
    );
}

#[test]
fn projection_root_epoch_compact_seals_archived_subtree() {
    let projection = project_spine_state_from_rollout(&[
        user_message("start"),
        spine_call("open-1", SPINE_TOOL_OPEN, "scope"),
        call_output("open-1"),
        compacted("Spine compacted root epoch 1 [0, 3)"),
        user_message("continue"),
        spine_call("open-2", SPINE_TOOL_OPEN, "post archive scope"),
        call_output("open-2"),
    ])
    .expect("project");

    assert_eq!(projection.response_item_count, 6);
    assert_eq!(projection.state.cursor(), &id(&[2, 1, 1]));
    assert_eq!(
        projection
            .state
            .node(&id(&[1]))
            .map(|node| node.status.clone()),
        Some(NodeStatus::Closed)
    );
    assert_eq!(
        projection
            .state
            .node(&id(&[1, 1]))
            .map(|node| node.status.clone()),
        Some(NodeStatus::Closed)
    );
    assert_eq!(
        projection
            .state
            .node(&id(&[1, 1, 1]))
            .map(|node| node.status.clone()),
        Some(NodeStatus::Finished)
    );
    assert_eq!(
        projection
            .state
            .nodes()
            .values()
            .filter(|node| node.status == NodeStatus::Live)
            .map(|node| node.node_id.clone())
            .collect::<Vec<_>>(),
        vec![id(&[2, 1, 1])]
    );
}
