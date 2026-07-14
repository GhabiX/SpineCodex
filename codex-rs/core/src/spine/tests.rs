use super::*;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::protocol::CompactedItem;
use codex_protocol::protocol::ThreadRolledBackEvent;
use codex_protocol::protocol::WorldStateItem;
use pretty_assertions::assert_eq;

fn message(role: &str, text: &str) -> RolloutItem {
    RolloutItem::ResponseItem(ResponseItem::Message {
        id: None,
        role: role.to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    })
}

fn call(call_id: &str, name: &str, arguments: &str) -> RolloutItem {
    RolloutItem::ResponseItem(ResponseItem::FunctionCall {
        id: None,
        name: name.to_string(),
        namespace: None,
        arguments: arguments.to_string(),
        call_id: call_id.to_string(),
        internal_chat_message_metadata_passthrough: None,
    })
}

fn namespaced_call(call_id: &str, namespace: &str, name: &str, arguments: &str) -> RolloutItem {
    RolloutItem::ResponseItem(ResponseItem::FunctionCall {
        id: None,
        name: name.to_string(),
        namespace: Some(namespace.to_string()),
        arguments: arguments.to_string(),
        call_id: call_id.to_string(),
        internal_chat_message_metadata_passthrough: None,
    })
}

fn output(call_id: &str, success: Option<bool>, text: &str) -> RolloutItem {
    RolloutItem::ResponseItem(ResponseItem::FunctionCallOutput {
        id: None,
        call_id: call_id.to_string(),
        output: FunctionCallOutputPayload {
            body: FunctionCallOutputBody::Text(text.to_string()),
            success,
        },
        internal_chat_message_metadata_passthrough: None,
    })
}

fn text(item: &ResponseItem) -> &str {
    let ResponseItem::Message { content, .. } = item else {
        panic!("expected message");
    };
    let ContentItem::InputText { text } = &content[0] else {
        panic!("expected input text");
    };
    text
}

#[test]
fn adapter_projects_open_and_close_from_native_function_carriers() {
    let rollout = vec![
        message("user", "request"),
        namespaced_call("open", "spine", "open", r#"{"summary":"task"}"#),
        output("open", Some(true), "Spine open accepted."),
        message("user", "detail"),
        call("close", "spine.close", r#"{"memory":"done"}"#),
        output("close", Some(true), "Spine close accepted."),
    ];

    let projection = derive_from_rollout(&rollout);
    assert_eq!(projection.spine.cursor.to_string(), "1");
    assert_eq!(projection.context.len(), 4);
    assert_eq!(text(&projection.context[0]), "[U1]\nrequest");
    assert!(text(&projection.context[1]).contains("# Spine Memory 1.1"));
    assert!(text(&projection.context[1]).contains("## User Message [U2]\ndetail"));
    assert!(text(&projection.context[1]).contains("## Node Memory\ndone"));
    assert!(matches!(
        projection.context[2],
        ResponseItem::FunctionCall { .. }
    ));
    assert!(matches!(
        projection.context[3],
        ResponseItem::FunctionCallOutput { .. }
    ));
}

#[test]
fn adapter_keeps_leading_assistant_and_multi_call_group_together() {
    let rollout = vec![
        message("assistant", "inspect first"),
        call("shell", "shell", r#"{"cmd":"pwd"}"#),
        call("open", "spine.open", r#"{"summary":"task"}"#),
        output("shell", Some(true), "workdir"),
        output("open", Some(true), "Spine open accepted."),
    ];

    let projection = derive_from_rollout(&rollout);
    assert_eq!(projection.spine.cursor.to_string(), "1.1");
    assert!(text(&projection.context[0]).starts_with("<spine_node"));
    assert_eq!(text(&projection.context[1]), "inspect first");
    assert_eq!(projection.context.len(), 6);
}

#[test]
fn failed_and_incomplete_control_outputs_do_not_transition() {
    let failed = vec![
        call("open", "spine.open", r#"{"summary":"task"}"#),
        output("open", Some(false), "failed"),
    ];
    let incomplete = vec![call("open", "spine.open", r#"{"summary":"task"}"#)];

    assert_eq!(derive_from_rollout(&failed).spine.cursor.to_string(), "1");
    assert_eq!(
        derive_from_rollout(&incomplete).spine.cursor.to_string(),
        "1"
    );
}

#[test]
fn compact_replacement_history_is_materialized_exactly_once() {
    let replacement = vec![ResponseItem::Message {
        id: Some("replacement".to_string()),
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: "native summary".to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }];
    let rollout = vec![
        message("user", "old"),
        RolloutItem::Compacted(CompactedItem {
            message: "summary".to_string(),
            replacement_history: Some(replacement.clone()),
            window_number: Some(1),
            first_window_id: None,
            previous_window_id: None,
            window_id: None,
        }),
    ];

    let projection = derive_from_rollout(&rollout);
    assert_eq!(projection.spine.cursor.to_string(), "2");
    assert_eq!(projection.context, replacement);
}

#[test]
fn rollback_rederives_from_surviving_native_prefix() {
    let rollout = vec![
        message("user", "first"),
        call("open", "spine.open", r#"{"summary":"first task"}"#),
        output("open", Some(true), "ok"),
        message("user", "second"),
        call("close", "spine.close", r#"{"memory":"done"}"#),
        output("close", Some(true), "ok"),
        RolloutItem::EventMsg(EventMsg::ThreadRolledBack(ThreadRolledBackEvent {
            num_turns: 1,
        })),
    ];

    let projection = derive_from_rollout(&rollout);
    assert_eq!(projection.spine.cursor.to_string(), "1.1");
    assert_eq!(projection.context.len(), 4);
    assert_eq!(text(&projection.context[0]), "[U1]\nfirst");
}

#[test]
fn fork_prefix_and_resume_full_rollout_are_pure_derivations() {
    let rollout = vec![
        message("user", "request"),
        call("open", "spine.open", r#"{"summary":"task"}"#),
        output("open", Some(true), "ok"),
        message("user", "detail"),
    ];
    let full = derive_from_rollout(&rollout);
    let resumed = derive_from_rollout(&rollout);
    let fork = derive_from_rollout(&rollout[..3]);
    assert_eq!(full, resumed);
    assert_eq!(fork.spine.cursor.to_string(), "1.1");
    assert_eq!(fork.context.len(), 4);
}

#[test]
fn non_context_rollout_records_do_not_change_response_ordinals() {
    let response_only = vec![
        message("user", "request"),
        call("open", "spine.open", r#"{"summary":"task"}"#),
        output("open", Some(true), "ok"),
    ];
    let with_metadata = vec![
        response_only[0].clone(),
        RolloutItem::WorldState(WorldStateItem {
            full: true,
            state: serde_json::json!({"cwd":"/tmp"}),
        }),
        response_only[1].clone(),
        response_only[2].clone(),
    ];

    assert_eq!(
        derive_from_rollout(&response_only),
        derive_from_rollout(&with_metadata)
    );
}

#[test]
fn multimodal_user_items_are_preserved_while_text_is_tagged() {
    let item = ResponseItem::Message {
        id: Some("multimodal".to_string()),
        role: "user".to_string(),
        content: vec![
            ContentItem::InputImage {
                image_url: "data:image/png;base64,abc".to_string(),
                detail: None,
            },
            ContentItem::InputText {
                text: "inspect image".to_string(),
            },
        ],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    };
    let projection = derive_from_rollout(&[RolloutItem::ResponseItem(item)]);
    let ResponseItem::Message { content, .. } = &projection.context[0] else {
        panic!("expected message");
    };
    assert!(matches!(content[0], ContentItem::InputImage { .. }));
    assert!(matches!(
        &content[1],
        ContentItem::InputText { text } if text == "[U1]\ninspect image"
    ));
}

#[test]
fn rollback_after_compact_keeps_native_replacement_baseline() {
    let replacement = vec![ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: "native summary".to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }];
    let rollout = vec![
        message("user", "first"),
        RolloutItem::Compacted(CompactedItem {
            message: "summary".to_string(),
            replacement_history: Some(replacement.clone()),
            window_number: Some(1),
            first_window_id: None,
            previous_window_id: None,
            window_id: None,
        }),
        message("user", "rolled back"),
        call("open", "spine.open", r#"{"summary":"discarded"}"#),
        output("open", Some(true), "ok"),
        RolloutItem::EventMsg(EventMsg::ThreadRolledBack(ThreadRolledBackEvent {
            num_turns: 1,
        })),
    ];

    let projection = derive_from_rollout(&rollout);
    assert_eq!(projection.spine.cursor.to_string(), "2");
    assert_eq!(projection.context, replacement);
}

#[test]
fn adapter_returns_materialized_context_without_persistence() {
    let rollout = vec![message("user", "request")];
    let projection = derive_from_rollout(&rollout);
    assert_eq!(projection.spine.cursor.to_string(), "1");
    assert_eq!(projection.context.len(), 1);
    assert_eq!(text(&projection.context[0]), "[U1]\nrequest");
}
