use super::*;
use codex_protocol::ResponseItemId;
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

fn response_items(effective: &[(usize, &RolloutItem)]) -> Vec<ResponseItem> {
    effective
        .iter()
        .filter_map(|(_, item)| match item {
            RolloutItem::ResponseItem(item) => Some(item.clone()),
            RolloutItem::InterAgentCommunication(communication) => {
                Some(communication.to_model_input_item())
            }
            RolloutItem::Compacted(_)
            | RolloutItem::SessionMeta(_)
            | RolloutItem::InterAgentCommunicationMetadata { .. }
            | RolloutItem::TurnContext(_)
            | RolloutItem::WorldState(_)
            | RolloutItem::EventMsg(_)
            | RolloutItem::SpineSamplingStarted(_)
            | RolloutItem::SpineTransition(_) => None,
        })
        .collect()
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
fn rollback_selected_prefix_trims_pre_turn_context_updates() {
    let rollout = vec![
        message(
            "developer",
            "<permissions instructions>base</permissions instructions>",
        ),
        message("user", "first"),
        message("assistant", "first response"),
        message(
            "developer",
            "<collaboration_mode>rolled back</collaboration_mode>",
        ),
        message("user", "second"),
        message("assistant", "second response"),
        RolloutItem::EventMsg(EventMsg::ThreadRolledBack(ThreadRolledBackEvent {
            num_turns: 1,
        })),
    ];

    let effective = effective_rollout(&rollout);
    assert_eq!(
        response_items(&effective),
        vec![
            match &rollout[0] {
                RolloutItem::ResponseItem(item) => item.clone(),
                _ => unreachable!(),
            },
            match &rollout[1] {
                RolloutItem::ResponseItem(item) => item.clone(),
                _ => unreachable!(),
            },
            match &rollout[2] {
                RolloutItem::ResponseItem(item) => item.clone(),
                _ => unreachable!(),
            },
        ]
    );
}

#[test]
fn non_context_rollout_records_do_not_change_source_ordinals() {
    let user = message("user", "request");
    let assistant = message("assistant", "answer");
    let response_only = vec![user.clone(), assistant.clone()];
    let with_metadata = vec![
        user,
        RolloutItem::WorldState(WorldStateItem {
            full: true,
            state: serde_json::json!({"cwd":"/tmp"}),
        }),
        assistant,
    ];

    let response_only = effective_rollout(&response_only)
        .into_iter()
        .map(|(ordinal, _)| ordinal)
        .collect::<Vec<_>>();
    let with_metadata = effective_rollout(&with_metadata)
        .into_iter()
        .map(|(ordinal, _)| ordinal)
        .collect::<Vec<_>>();
    assert_eq!(response_only, vec![0, 1]);
    assert_eq!(with_metadata, response_only);
}

#[test]
fn user_message_projection_entries_follow_effective_rollout() {
    let rollout = vec![
        message("user", "<environment_context>context</environment_context>"),
        message("user", "first"),
        message("assistant", "answer"),
        message("user", "second"),
    ];

    assert_eq!(
        user_message_projection_entries(&rollout),
        vec![
            memory_projection::SpinetreeUserMessageProjectionEntry {
                anchor: 1,
                body: "first".to_string(),
            },
            memory_projection::SpinetreeUserMessageProjectionEntry {
                anchor: 2,
                body: "second".to_string(),
            },
        ]
    );
}

#[test]
fn source_span_materializes_native_request_and_output_in_order() {
    let request = ResponseItem::FunctionCall {
        id: Some(ResponseItemId::from_server("request".to_string())),
        name: "shell".to_string(),
        namespace: None,
        arguments: r#"{"cmd":"pwd"}"#.to_string(),
        call_id: "call".to_string(),
        encrypted_function_args: None,
        internal_chat_message_metadata_passthrough: None,
    };
    let output = ResponseItem::FunctionCallOutput {
        id: Some(ResponseItemId::from_server("output".to_string())),
        call_id: "call".to_string(),
        output: FunctionCallOutputPayload::from_text("/tmp".to_string()),
        internal_chat_message_metadata_passthrough: None,
    };
    let rollout = vec![
        RolloutItem::ResponseItem(request.clone()),
        RolloutItem::ResponseItem(output.clone()),
    ];
    let effective = effective_rollout(&rollout);

    assert_eq!(
        materialize_context(
            &[ContextItem::SourceSpan {
                span: spine_core::RawSpan {
                    start: RawBoundary(0),
                    end: RawBoundary(1),
                },
            }],
            &effective,
            None,
            None,
            &BTreeMap::new(),
            "node prompt",
        )
        .expect("materialize source span"),
        vec![request, output]
    );
}

#[test]
fn multimodal_user_item_is_preserved_while_text_is_anchored() {
    let item = ResponseItem::Message {
        id: Some(ResponseItemId::from_server("multimodal".to_string())),
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
    let rollout = vec![RolloutItem::ResponseItem(item.clone())];
    let effective = effective_rollout(&rollout);
    let projected = materialize_context(
        &[ContextItem::Message {
            message: message_from_response_item(0, &item),
            user_anchor: Some(1),
        }],
        &effective,
        None,
        None,
        &BTreeMap::new(),
        "node prompt",
    )
    .expect("materialize user");

    let ResponseItem::Message { content, .. } = &projected[0] else {
        panic!("expected message");
    };
    assert!(matches!(content[0], ContentItem::InputImage { .. }));
    assert!(matches!(
        &content[1],
        ContentItem::InputText { text } if text == "[U1]\ninspect image"
    ));
}

#[test]
fn contextual_user_message_does_not_consume_an_anchor() {
    let contextual = match message("user", "<environment_context>context</environment_context>") {
        RolloutItem::ResponseItem(item) => item,
        _ => unreachable!(),
    };
    let request = match message("user", "actual request") {
        RolloutItem::ResponseItem(item) => item,
        _ => unreachable!(),
    };
    let rollout = vec![
        RolloutItem::ResponseItem(contextual.clone()),
        RolloutItem::ResponseItem(request.clone()),
    ];
    let effective = effective_rollout(&rollout);
    let projected = materialize_context(
        &[
            ContextItem::Message {
                message: message_from_response_item(0, &contextual),
                user_anchor: None,
            },
            ContextItem::Message {
                message: message_from_response_item(1, &request),
                user_anchor: Some(1),
            },
        ],
        &effective,
        None,
        None,
        &BTreeMap::new(),
        "node prompt",
    )
    .expect("materialize contextual user");

    assert_eq!(
        text(&projected[0]),
        "<environment_context>context</environment_context>"
    );
    assert_eq!(text(&projected[1]), "[U1]\nactual request");
}

#[test]
fn compact_replacement_history_is_materialized_exactly_once() {
    let replacement = ResponseItem::Message {
        id: Some(ResponseItemId::from_server("replacement".to_string())),
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: "native summary".to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    };
    let rollout = vec![
        message("user", "old"),
        RolloutItem::Compacted(CompactedItem {
            message: "summary".to_string(),
            replacement_history: Some(vec![replacement.clone()]),
            window_number: Some(1),
            first_window_id: None,
            previous_window_id: None,
            window_id: None,
        }),
    ];
    let effective = effective_rollout(&rollout);

    assert_eq!(
        materialize_context(
            &[ContextItem::Native {
                source: NativeItemRef::CompactReplacement {
                    compact_boundary: RawBoundary(1),
                    index: 0,
                },
            }],
            &effective,
            None,
            None,
            &BTreeMap::new(),
            "node prompt",
        )
        .expect("materialize compact replacement"),
        vec![replacement]
    );
}

#[test]
fn closed_memory_user_slot_preserves_the_complete_native_message() {
    let item = ResponseItem::Message {
        id: Some(ResponseItemId::from_server("multimodal-memory".to_string())),
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
    let rollout = vec![RolloutItem::ResponseItem(item.clone())];
    let effective = effective_rollout(&rollout);
    let owner = spine_core::NodeId::root_epoch(1).child(1);
    let projected = materialize_context(
        &[
            ContextItem::MemorySlot(MemorySlot::User {
                owner_node: owner.clone(),
                message: message_from_response_item(0, &item),
                anchor: 1,
            }),
            ContextItem::MemorySlot(MemorySlot::Summary {
                owner_node: owner,
                source: spine_core::RawSpan {
                    start: RawBoundary(1),
                    end: RawBoundary(2),
                },
                body: "image inspected".to_string(),
            }),
        ],
        &effective,
        None,
        None,
        &BTreeMap::new(),
        "node prompt",
    )
    .expect("materialize memory");

    let mut expected = item;
    SpineUserAnchor::new(1).apply(&mut expected);
    assert_eq!(projected[0], expected);
    assert_eq!(
        text(&projected[1]),
        "<spine_memory node_id=\"1.1\">\nimage inspected\n</spine_memory>"
    );
}
