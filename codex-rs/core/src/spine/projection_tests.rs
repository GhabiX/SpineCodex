use super::*;
use crate::spine::projection_epoch::ProjectionEpochClassification;
use crate::spine::projection_epoch::classify_projection_epoch;
use crate::spine::projection_epoch::projection_rollout_position;
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
    let arguments = if op == SPINE_TOOL_OPEN {
        "{}".to_string()
    } else {
        format!(r#"{{"summary":"{summary}"}}"#)
    };
    spine_call_with_args(call_id, op, &arguments)
}

fn spine_close_call(call_id: &str, child_summary: &str, summary: &str) -> RolloutItem {
    spine_call_with_args(
        call_id,
        SPINE_TOOL_CLOSE,
        &format!(r#"{{"child_summary":"{child_summary}","summary":"{summary}"}}"#),
    )
}

fn spine_call_with_args(call_id: &str, op: &str, arguments: &str) -> RolloutItem {
    RolloutItem::ResponseItem(ResponseItem::FunctionCall {
        id: None,
        name: op.to_string(),
        namespace: Some(SPINE_NAMESPACE.to_string()),
        arguments: arguments.to_string(),
        call_id: call_id.to_string(),
    })
}

fn legacy_spine_call(call_id: &str, arguments: &str) -> RolloutItem {
    RolloutItem::ResponseItem(ResponseItem::FunctionCall {
        id: None,
        name: "spine".to_string(),
        namespace: None,
        arguments: arguments.to_string(),
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
fn projects_namespaced_close_with_child_summary() {
    let projection = project_spine_state_from_rollout(&[
        user_message("start"),
        spine_call("open-1", SPINE_TOOL_OPEN, "scope"),
        call_output("open-1"),
        spine_close_call("close-1", "leaf done", "scope done"),
        call_output("close-1"),
    ])
    .expect("project");

    assert_eq!(projection.response_item_count, 5);
    assert_eq!(projection.state.cursor().to_string(), "1.2");
    assert_eq!(
        projection
            .state
            .node(&id(&[1, 1, 1]))
            .expect("child node")
            .summary
            .as_deref(),
        Some("leaf done")
    );
    assert_eq!(
        projection
            .state
            .node(&id(&[1, 1]))
            .expect("parent node")
            .summary
            .as_deref(),
        Some("scope done")
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
fn projection_rolls_back_namespaced_close_with_child_summary() {
    let projection = project_spine_state_from_rollout(&[
        turn_started("turn-1"),
        user_message("start"),
        spine_call("open-1", SPINE_TOOL_OPEN, "scope"),
        call_output("open-1"),
        turn_complete("turn-1"),
        turn_started("rolled-back-turn"),
        user_message("drop close"),
        spine_close_call("close-1", "rolled leaf", "rolled scope"),
        call_output("close-1"),
        turn_complete("rolled-back-turn"),
        RolloutItem::EventMsg(EventMsg::ThreadRolledBack(ThreadRolledBackEvent {
            num_turns: 1,
        })),
    ])
    .expect("project");

    assert_eq!(projection.response_item_count, 3);
    assert_eq!(projection.state.cursor().to_string(), "1.1.1");
    assert_eq!(
        projection
            .state
            .node(&id(&[1, 1]))
            .expect("parent node")
            .summary
            .as_deref(),
        None
    );
}

#[test]
fn non_spine_compact_stop_projection_ignores_later_spine_transitions() {
    let projection = project_spine_state_from_rollout(&[
        user_message("start"),
        spine_call("open-1", SPINE_TOOL_OPEN, "scope"),
        call_output("open-1"),
        compacted("native compact summary"),
        user_message("after native compact"),
        spine_call("next-after-stop", SPINE_TOOL_NEXT, "must not project"),
        call_output("next-after-stop"),
    ])
    .expect("project");

    assert_eq!(projection.response_item_count, 3);
    assert_eq!(projection.state.cursor(), &id(&[1, 1, 1]));
    assert!(
        projection.state.node(&id(&[1, 2])).is_none(),
        "native compact Stop boundary must not admit later Spine transitions"
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
fn projection_epoch_metadata_classifies_resume_position() {
    let source_items = vec![
        turn_started("turn-1"),
        user_message("start"),
        spine_call("open-1", SPINE_TOOL_OPEN, "scope"),
        call_output("open-1"),
        turn_complete("turn-1"),
    ];
    let projection = project_spine_state_from_rollout_with_source("rollout-a.jsonl", &source_items)
        .expect("project");

    assert_eq!(projection.epoch.source_rollout_ref, "rollout-a.jsonl");
    assert_eq!(projection.epoch.processed_rollout_len, 5);
    assert_eq!(
        projection.epoch.effective_raw_len,
        projection.response_item_count
    );
    assert!(
        projection
            .epoch
            .processed_rollout_hash
            .starts_with("sha256:")
    );
    assert!(
        projection
            .epoch
            .surviving_turn_ids_hash
            .starts_with("sha256:")
    );
    assert!(projection.epoch.state_hash.starts_with("sha256:"));

    let current = projection_rollout_position("rollout-a.jsonl", &source_items)
        .expect("current rollout position");
    assert_eq!(
        classify_projection_epoch(&projection.epoch, &current, 5),
        ProjectionEpochClassification::Current
    );

    let mut longer_items = source_items.clone();
    longer_items.push(user_message("new turn"));
    let same_prefix = projection_rollout_position("rollout-a.jsonl", &longer_items[..5])
        .expect("prefix rollout position");
    assert_eq!(
        classify_projection_epoch(&projection.epoch, &same_prefix, 6),
        ProjectionEpochClassification::Behind
    );
    assert_eq!(
        classify_projection_epoch(&projection.epoch, &current, 4),
        ProjectionEpochClassification::Ahead
    );

    let divergent_items = vec![user_message("different")];
    let divergent = projection_rollout_position("rollout-a.jsonl", &divergent_items)
        .expect("divergent rollout position");
    assert_eq!(
        classify_projection_epoch(&projection.epoch, &divergent, 5),
        ProjectionEpochClassification::Divergent
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

#[test]
fn legacy_spine_transition_is_still_guarded_for_resume_compatibility() {
    let projection = project_spine_state_from_rollout(&[
        user_message("start"),
        legacy_spine_call("legacy-open", r#"{"op":"open"}"#),
        call_output("legacy-open"),
    ])
    .expect("legacy transition should project for resume compatibility");

    assert_eq!(projection.state.cursor(), &id(&[1, 1, 1]));

    let err = project_spine_state_from_rollout(&[
        user_message("start"),
        legacy_spine_call("legacy-bad", r#"{"op":"tree","summary":"bad"}"#),
    ])
    .expect_err("unknown legacy transition op must remain guarded");
    assert!(matches!(err, SpineProjectionError::UnknownLegacyOp { .. }));
}
