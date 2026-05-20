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
use codex_protocol::protocol::SpineCompactedCheckpoint;
use codex_protocol::protocol::SpineCompactedCheckpointKind;
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

fn spine_call_with_args(call_id: &str, op: &str, arguments: &str) -> RolloutItem {
    RolloutItem::ResponseItem(ResponseItem::FunctionCall {
        id: None,
        name: op.to_string(),
        namespace: Some(SPINE_NAMESPACE.to_string()),
        arguments: arguments.to_string(),
        call_id: call_id.to_string(),
    })
}

fn unnamespaced_spine_call(call_id: &str, arguments: &str) -> RolloutItem {
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
        spine: None,
    })
}

fn spine_compacted(compact_id: &str, kind: SpineCompactedCheckpointKind) -> RolloutItem {
    RolloutItem::Compacted(CompactedItem {
        message: format!("checkpoint {compact_id}"),
        replacement_history: Some(Vec::new()),
        spine: Some(SpineCompactedCheckpoint {
            compact_id: compact_id.to_string(),
            kind,
        }),
    })
}

fn contextual_dev_message(text: &str) -> RolloutItem {
    RolloutItem::ResponseItem(ResponseItem::Message {
        id: None,
        role: "developer".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
    })
}

fn contextual_user_message(text: &str) -> RolloutItem {
    RolloutItem::ResponseItem(ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
    })
}

#[test]
fn projects_committed_spine_transitions_from_rollout_prefix() {
    let projection = project_spine_state_from_rollout(&[
        user_message("start"),
        spine_call("open-1", SPINE_TOOL_OPEN, "scope"),
        call_output("open-1"),
        spine_call("close-1", SPINE_TOOL_CLOSE, "done"),
        call_output("close-1"),
        spine_call("open-2", SPINE_TOOL_OPEN, "next scope"),
        call_output("open-2"),
    ])
    .expect("project");

    assert_eq!(projection.response_item_count, 7);
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
fn projection_keeps_initial_context_prelude_out_of_root_epoch_cut() {
    let projection = project_spine_state_from_rollout(&[
        contextual_dev_message("developer instructions prelude"),
        contextual_user_message("<environment_context>\nprelude\n</environment_context>"),
        user_message("real turn"),
        spine_compacted("compact-root", SpineCompactedCheckpointKind::RootEpoch),
    ])
    .expect("project");

    assert_eq!(projection.response_item_count, 3);
    assert_eq!(
        projection
            .state
            .node(&id(&[1]))
            .and_then(|node| node.raw_start_ordinal),
        Some(2)
    );
    assert_eq!(
        projection
            .state
            .node(&id(&[2, 1]))
            .and_then(|node| node.raw_start_ordinal),
        Some(3)
    );
    assert_eq!(
        projection.checkpoint.replay().expect("checkpoint replay"),
        projection.state
    );
}

#[test]
fn projection_does_not_treat_plain_first_user_message_as_prelude() {
    let projection = project_spine_state_from_rollout(&[
        user_message("real turn"),
        spine_compacted("compact-root", SpineCompactedCheckpointKind::RootEpoch),
    ])
    .expect("project");

    assert_eq!(
        projection
            .state
            .node(&id(&[1]))
            .and_then(|node| node.raw_start_ordinal),
        Some(0)
    );
    assert_eq!(
        projection
            .state
            .node(&id(&[2, 1]))
            .and_then(|node| node.raw_start_ordinal),
        Some(1)
    );
}

#[test]
fn projects_namespaced_close_to_parent() {
    let projection = project_spine_state_from_rollout(&[
        user_message("start"),
        spine_call("open-1", SPINE_TOOL_OPEN, "scope"),
        call_output("open-1"),
        spine_call("close-1", SPINE_TOOL_CLOSE, "leaf done"),
        call_output("close-1"),
    ])
    .expect("project");

    assert_eq!(projection.response_item_count, 5);
    assert_eq!(projection.state.cursor().to_string(), "1.1");
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
        None
    );
}

#[test]
fn projection_applies_thread_rollback_markers() {
    let projection = project_spine_state_from_rollout(&[
        user_message("start"),
        spine_call("open-1", SPINE_TOOL_OPEN, "scope"),
        call_output("open-1"),
        user_message("drop this turn"),
        spine_call("close-1", SPINE_TOOL_CLOSE, "rolled back"),
        call_output("close-1"),
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
fn projection_rolls_back_namespaced_close() {
    let projection = project_spine_state_from_rollout(&[
        turn_started("turn-1"),
        user_message("start"),
        spine_call("open-1", SPINE_TOOL_OPEN, "scope"),
        call_output("open-1"),
        turn_complete("turn-1"),
        turn_started("rolled-back-turn"),
        user_message("drop close"),
        spine_call("close-1", SPINE_TOOL_CLOSE, "rolled leaf"),
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
        spine_call("unknown-after-stop", "unknown", "must not project"),
        call_output("unknown-after-stop"),
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
        spine_call("close-1", SPINE_TOOL_CLOSE, "rolled back"),
        call_output("close-1"),
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
fn projection_keeps_only_surviving_compact_ids_after_rollback() {
    let projection = project_spine_state_from_rollout(&[
        turn_started("turn-1"),
        user_message("start"),
        spine_compacted("compact-surviving", SpineCompactedCheckpointKind::Suffix),
        turn_complete("turn-1"),
        turn_started("rolled-back-turn"),
        user_message("redo this turn"),
        spine_compacted("compact-rolled-back", SpineCompactedCheckpointKind::Suffix),
        turn_complete("rolled-back-turn"),
        RolloutItem::EventMsg(EventMsg::ThreadRolledBack(ThreadRolledBackEvent {
            num_turns: 1,
        })),
    ])
    .expect("project");

    assert!(
        projection
            .surviving_compact_ids
            .contains("compact-surviving")
    );
    assert!(
        !projection
            .surviving_compact_ids
            .contains("compact-rolled-back")
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
    assert!(projection.epoch.checkpoint_hash.starts_with("sha256:"));

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
        spine_compacted("compact-root", SpineCompactedCheckpointKind::RootEpoch),
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
        Some(NodeStatus::Closed)
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
fn unnamespaced_spine_call_is_ignored_by_projection() {
    let projection = project_spine_state_from_rollout(&[
        user_message("start"),
        unnamespaced_spine_call("plain-spine", r#"{"op":"open"}"#),
        call_output("plain-spine"),
    ])
    .expect("project");

    assert_eq!(projection.state.cursor(), &id(&[1, 1]));
    assert_eq!(projection.response_item_count, 3);
}
