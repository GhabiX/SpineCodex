use super::*;
use pretty_assertions::assert_eq;

fn boundary(value: u64) -> RawBoundary {
    RawBoundary(value)
}

fn message(value: u64, role: MessageRole, content: &str) -> RolloutEvent {
    RolloutEvent::Message(Message {
        boundary: boundary(value),
        role,
        content: content.to_string(),
    })
}

fn tool_use(name: &str, arguments: &str, outcome: Option<ToolOutcome>) -> ToolUse {
    ToolUse {
        call_id: format!("call-{name}"),
        name: name.to_string(),
        arguments: arguments.to_string(),
        outcome,
        output: outcome.map(|_| format!("{name} output")),
    }
}

fn group(value: u64, calls: Vec<ToolUse>) -> ToolCallGroup {
    ToolCallGroup {
        start: boundary(value),
        end: boundary(value + 1),
        leading_assistant_messages: Vec::new(),
        calls,
    }
}

fn ordinary_group(value: u64) -> RolloutEvent {
    RolloutEvent::ToolCall(group(
        value,
        vec![tool_use(
            "shell",
            r#"{"cmd":"pwd"}"#,
            Some(ToolOutcome::Succeeded),
        )],
    ))
}

fn open(value: u64, summary: &str) -> RolloutEvent {
    RolloutEvent::ToolCall(group(
        value,
        vec![tool_use(
            "spine.open",
            &serde_json::json!({"summary": summary}).to_string(),
            Some(ToolOutcome::Succeeded),
        )],
    ))
}

fn close(value: u64, memory: &str) -> RolloutEvent {
    RolloutEvent::ToolCall(group(
        value,
        vec![tool_use(
            "spine.close",
            &serde_json::json!({"memory": memory}).to_string(),
            Some(ToolOutcome::Succeeded),
        )],
    ))
}

fn next(value: u64, summary: &str, memory: &str) -> RolloutEvent {
    RolloutEvent::ToolCall(group(
        value,
        vec![tool_use(
            "spine.next",
            &serde_json::json!({"summary": summary, "memory": memory}).to_string(),
            Some(ToolOutcome::Succeeded),
        )],
    ))
}

fn compact(value: u64, replacement_history: Vec<ContextItem>) -> RolloutEvent {
    RolloutEvent::Compact {
        boundary: boundary(value),
        replacement_history,
    }
}

fn apply(events: &[RolloutEvent]) -> SpineProjection {
    let mut reducer = SpineReducer::new();
    for event in events {
        reducer.apply(event.clone());
    }
    reducer.projection()
}

fn node<'a>(projection: &'a SpineProjection, id: &str) -> &'a NodeSnapshot {
    projection
        .nodes
        .iter()
        .find(|node| node.id.to_string() == id)
        .unwrap_or_else(|| panic!("missing node {id}"))
}

fn only_memory(context: &[ContextItem]) -> (&NodeId, &[MemoryPart]) {
    let [ContextItem::Memory { node_id, parts }] = context else {
        panic!("expected one memory item, got {context:#?}");
    };
    (node_id, parts)
}

#[test]
fn init_creates_only_root_epoch() {
    let projection = SpineReducer::new().projection();
    assert_eq!(projection.cursor.to_string(), "1");
    assert_eq!(projection.nodes.len(), 1);
    assert_eq!(projection.nodes[0].kind, NodeKind::RootEpoch);
    assert_eq!(projection.visible_context, Vec::<ContextItem>::new());
}

#[test]
fn user_message_gets_stable_anchor() {
    let projection = apply(&[message(1, MessageRole::User, "request")]);
    let [ContextItem::Message { user_anchor, .. }] = projection.visible_context.as_slice() else {
        panic!("expected one message");
    };
    assert_eq!(*user_anchor, Some(1));
}

#[test]
fn assistant_message_has_no_user_anchor() {
    let projection = apply(&[message(1, MessageRole::Assistant, "answer")]);
    let [ContextItem::Message { user_anchor, .. }] = projection.visible_context.as_slice() else {
        panic!("expected one message");
    };
    assert_eq!(*user_anchor, None);
}

#[test]
fn user_anchor_sequence_ignores_non_user_messages() {
    let projection = apply(&[
        message(1, MessageRole::User, "one"),
        message(2, MessageRole::Assistant, "middle"),
        message(3, MessageRole::User, "two"),
    ]);
    let anchors: Vec<_> = projection
        .visible_context
        .iter()
        .filter_map(|item| match item {
            ContextItem::Message { user_anchor, .. } => *user_anchor,
            _ => None,
        })
        .collect();
    assert_eq!(anchors, vec![1, 2]);
}

#[test]
fn ordinary_toolcall_is_one_leaf() {
    let projection = apply(&[ordinary_group(1)]);
    assert!(matches!(
        projection.visible_context.as_slice(),
        [ContextItem::ToolCall(_)]
    ));
}

#[test]
fn leading_assistant_messages_remain_inside_tool_group() {
    let mut event = group(
        2,
        vec![tool_use("shell", "{}", Some(ToolOutcome::Succeeded))],
    );
    event.leading_assistant_messages.push(Message {
        boundary: boundary(1),
        role: MessageRole::Assistant,
        content: "I will inspect this.".to_string(),
    });
    let projection = apply(&[RolloutEvent::ToolCall(event)]);
    let [ContextItem::ToolCall(group)] = projection.visible_context.as_slice() else {
        panic!("expected one tool group");
    };
    assert_eq!(group.leading_assistant_messages.len(), 1);
}

#[test]
fn incomplete_control_group_is_ordinary() {
    let projection = apply(&[RolloutEvent::ToolCall(group(
        1,
        vec![tool_use("spine.open", r#"{"summary":"child"}"#, None)],
    ))]);
    assert_eq!(projection.cursor.to_string(), "1");
    assert!(matches!(
        projection.visible_context.as_slice(),
        [ContextItem::ToolCall(_)]
    ));
}

#[test]
fn failed_control_group_is_ordinary() {
    let projection = apply(&[RolloutEvent::ToolCall(group(
        1,
        vec![tool_use(
            "spine.open",
            r#"{"summary":"child"}"#,
            Some(ToolOutcome::Failed),
        )],
    ))]);
    assert_eq!(projection.cursor.to_string(), "1");
    assert_eq!(projection.nodes.len(), 1);
}

#[test]
fn unknown_control_outcome_is_ordinary() {
    let projection = apply(&[RolloutEvent::ToolCall(group(
        1,
        vec![tool_use(
            "spine.open",
            r#"{"summary":"child"}"#,
            Some(ToolOutcome::Unknown),
        )],
    ))]);
    assert_eq!(projection.nodes.len(), 1);
}

#[test]
fn malformed_control_arguments_are_ordinary() {
    let projection = apply(&[RolloutEvent::ToolCall(group(
        1,
        vec![tool_use(
            "spine.open",
            "not-json",
            Some(ToolOutcome::Succeeded),
        )],
    ))]);
    assert_eq!(projection.nodes.len(), 1);
}

#[test]
fn unknown_control_fields_are_rejected() {
    let projection = apply(&[RolloutEvent::ToolCall(group(
        1,
        vec![tool_use(
            "spine.open",
            r#"{"summary":"child","extra":true}"#,
            Some(ToolOutcome::Succeeded),
        )],
    ))]);
    assert_eq!(projection.nodes.len(), 1);
}

#[test]
fn empty_open_summary_is_ordinary() {
    let projection = apply(&[open(1, "  \n")]);
    assert_eq!(projection.nodes.len(), 1);
}

#[test]
fn open_creates_child_and_moves_cursor() {
    let projection = apply(&[open(1, " child ")]);
    assert_eq!(projection.cursor.to_string(), "1.1");
    assert_eq!(node(&projection, "1").children[0].to_string(), "1.1");
    assert_eq!(node(&projection, "1.1").summary.as_deref(), Some("child"));
}

#[test]
fn open_group_belongs_to_new_child() {
    let projection = apply(&[open(1, "child")]);
    assert!(matches!(
        projection.visible_context.as_slice(),
        [ContextItem::SyntheticNode { .. }, ContextItem::ToolCall(_)]
    ));
}

#[test]
fn nested_open_creates_hierarchical_id() {
    let projection = apply(&[open(1, "parent"), open(3, "child")]);
    assert_eq!(projection.cursor.to_string(), "1.1.1");
    assert_eq!(node(&projection, "1.1").status, NodeStatus::Opened);
}

#[test]
fn close_at_root_is_ordinary() {
    let projection = apply(&[close(1, "invalid root memory")]);
    assert_eq!(projection.cursor.to_string(), "1");
    assert_eq!(node(&projection, "1").status, NodeStatus::Live);
    assert!(matches!(
        projection.visible_context.as_slice(),
        [ContextItem::ToolCall(_)]
    ));
}

#[test]
fn close_moves_cursor_to_parent() {
    let projection = apply(&[open(1, "child"), close(3, "done")]);
    assert_eq!(projection.cursor.to_string(), "1");
    assert_eq!(node(&projection, "1.1").status, NodeStatus::Closed);
    assert_eq!(node(&projection, "1").status, NodeStatus::Live);
}

#[test]
fn close_uses_group_start_as_end_boundary() {
    let projection = apply(&[open(1, "child"), close(10, "done")]);
    assert_eq!(node(&projection, "1.1").end, Some(boundary(10)));
}

#[test]
fn close_memory_ends_with_model_memory() {
    let projection = apply(&[open(1, "child"), close(3, "model memory")]);
    let task = node(&projection, "1.1");
    assert_eq!(
        task.memory.as_deref(),
        Some([MemoryPart::Model("model memory".to_string())].as_slice())
    );
}

#[test]
fn close_memory_preserves_direct_user_messages() {
    let projection = apply(&[
        open(1, "child"),
        message(3, MessageRole::User, "request"),
        close(4, "done"),
    ]);
    assert_eq!(
        node(&projection, "1.1").memory,
        Some(vec![
            MemoryPart::User {
                anchor: 1,
                content: "request".to_string(),
            },
            MemoryPart::Model("done".to_string()),
        ])
    );
}

#[test]
fn fake_user_anchor_in_model_memory_selects_no_evidence() {
    let projection = apply(&[open(1, "child"), close(3, "remember [U99]")]);
    assert_eq!(
        node(&projection, "1.1").memory,
        Some(vec![MemoryPart::Model("remember [U99]".to_string())])
    );
}

#[test]
fn parent_memory_preserves_child_memory_in_source_order() {
    let projection = apply(&[
        open(1, "parent"),
        message(3, MessageRole::User, "before"),
        open(4, "child"),
        message(6, MessageRole::User, "inside"),
        close(7, "child done"),
        message(9, MessageRole::User, "after"),
        close(10, "parent done"),
    ]);
    let memory = node(&projection, "1.1").memory.as_ref().unwrap();
    assert_eq!(
        memory,
        &vec![
            MemoryPart::User {
                anchor: 1,
                content: "before".to_string(),
            },
            MemoryPart::Child {
                node_id: NodeId::root_epoch(1).child(1).child(1),
                parts: vec![
                    MemoryPart::User {
                        anchor: 2,
                        content: "inside".to_string(),
                    },
                    MemoryPart::Model("child done".to_string()),
                ],
            },
            MemoryPart::User {
                anchor: 3,
                content: "after".to_string(),
            },
            MemoryPart::Model("parent done".to_string()),
        ]
    );
}

#[test]
fn close_projects_memory_then_group_in_parent() {
    let projection = apply(&[open(1, "child"), close(3, "done")]);
    assert!(matches!(
        projection.visible_context.as_slice(),
        [ContextItem::Memory { .. }, ContextItem::ToolCall(_)]
    ));
}

#[test]
fn next_closes_current_and_opens_sibling() {
    let projection = apply(&[open(1, "first"), next(3, "second", "first done")]);
    assert_eq!(projection.cursor.to_string(), "1.2");
    assert_eq!(node(&projection, "1.1").status, NodeStatus::Closed);
    assert_eq!(node(&projection, "1.2").status, NodeStatus::Live);
}

#[test]
fn next_group_belongs_to_new_sibling() {
    let projection = apply(&[open(1, "first"), next(3, "second", "first done")]);
    assert!(matches!(
        projection.visible_context.as_slice(),
        [
            ContextItem::Memory { .. },
            ContextItem::SyntheticNode { .. },
            ContextItem::ToolCall(_)
        ]
    ));
}

#[test]
fn next_memory_is_stored_on_closed_node() {
    let projection = apply(&[open(1, "first"), next(3, "second", "first done")]);
    assert_eq!(
        node(&projection, "1.1").memory,
        Some(vec![MemoryPart::Model("first done".to_string())])
    );
}

#[test]
fn conflicting_successful_controls_apply_no_transition() {
    let projection = apply(&[RolloutEvent::ToolCall(group(
        1,
        vec![
            tool_use(
                "spine.open",
                r#"{"summary":"child"}"#,
                Some(ToolOutcome::Succeeded),
            ),
            tool_use(
                "spine.close",
                r#"{"memory":"done"}"#,
                Some(ToolOutcome::Succeeded),
            ),
        ],
    ))]);
    assert_eq!(projection.nodes.len(), 1);
    assert!(matches!(
        projection.visible_context.as_slice(),
        [ContextItem::ToolCall(_)]
    ));
}

#[test]
fn ordinary_call_can_coexist_with_one_control() {
    let projection = apply(&[RolloutEvent::ToolCall(group(
        1,
        vec![
            tool_use("shell", "{}", Some(ToolOutcome::Succeeded)),
            tool_use(
                "spine.open",
                r#"{"summary":"child"}"#,
                Some(ToolOutcome::Succeeded),
            ),
        ],
    ))]);
    assert_eq!(projection.cursor.to_string(), "1.1");
    let ContextItem::ToolCall(group) = &projection.visible_context[1] else {
        panic!("expected complete group in child");
    };
    assert_eq!(group.calls.len(), 2);
}

#[test]
fn compact_creates_next_root_epoch() {
    let projection = apply(&[compact(4, Vec::new())]);
    assert_eq!(projection.cursor.to_string(), "2");
    assert_eq!(node(&projection, "1").status, NodeStatus::Compacted);
    assert_eq!(node(&projection, "2").status, NodeStatus::Live);
}

#[test]
fn compact_replacement_history_is_new_visible_baseline() {
    let baseline = vec![ContextItem::Message {
        message: Message {
            boundary: boundary(4),
            role: MessageRole::Assistant,
            content: "native summary".to_string(),
        },
        user_anchor: None,
    }];
    let projection = apply(&[
        message(1, MessageRole::User, "old"),
        compact(4, baseline.clone()),
    ]);
    assert_eq!(projection.visible_context, baseline);
}

#[test]
fn compact_does_not_reapply_old_closed_memory() {
    let baseline = vec![ContextItem::Message {
        message: Message {
            boundary: boundary(8),
            role: MessageRole::Assistant,
            content: "summary includes old work".to_string(),
        },
        user_anchor: None,
    }];
    let projection = apply(&[
        open(1, "child"),
        close(3, "old memory"),
        compact(8, baseline.clone()),
    ]);
    assert_eq!(projection.visible_context, baseline);
}

#[test]
fn compact_marks_nested_live_path_compacted() {
    let projection = apply(&[open(1, "parent"), open(3, "child"), compact(8, Vec::new())]);
    assert_eq!(node(&projection, "1.1").status, NodeStatus::Compacted);
    assert_eq!(node(&projection, "1.1.1").status, NodeStatus::Compacted);
}

#[test]
fn closed_nodes_remain_closed_across_compact() {
    let projection = apply(&[open(1, "child"), close(3, "done"), compact(8, Vec::new())]);
    assert_eq!(node(&projection, "1.1").status, NodeStatus::Closed);
}

#[test]
fn context_delta_reconstructs_each_projection() {
    let events = [
        message(1, MessageRole::User, "request"),
        open(2, "child"),
        close(4, "done"),
    ];
    let mut reducer = SpineReducer::new();
    let mut installed = Vec::new();
    for event in events {
        let delta = reducer.apply(event);
        delta.context_edit.apply(&mut installed);
        assert_eq!(installed, delta.projection.visible_context);
    }
}

#[test]
fn full_derive_equals_incremental_projection() {
    let events = vec![
        message(1, MessageRole::User, "request"),
        open(2, "parent"),
        ordinary_group(4),
        next(6, "sibling", "parent done"),
        close(8, "sibling done"),
    ];
    assert_eq!(SpineReducer::derive(&events), apply(&events));
}

#[test]
fn every_rollout_prefix_replays_to_incremental_state() {
    let events = vec![
        message(1, MessageRole::User, "request"),
        open(2, "parent"),
        message(4, MessageRole::User, "detail"),
        open(5, "child"),
        ordinary_group(7),
        close(9, "child done"),
        next(11, "sibling", "parent done"),
        close(13, "sibling done"),
        compact(15, Vec::new()),
    ];
    let mut incremental = SpineReducer::new();
    assert_eq!(incremental.projection(), SpineReducer::derive(&[]));
    for (index, event) in events.iter().enumerate() {
        incremental.apply(event.clone());
        assert_eq!(
            incremental.projection(),
            SpineReducer::derive(&events[..=index]),
            "prefix ending at event {index}"
        );
    }
}

#[test]
fn bounded_event_space_preserves_prefix_replay_equivalence() {
    let alphabet = [
        message(1, MessageRole::User, "request"),
        message(1, MessageRole::Assistant, "answer"),
        ordinary_group(1),
        open(1, "child"),
        close(1, "done"),
        next(1, "sibling", "done"),
        compact(1, Vec::new()),
    ];
    let sequence_len = 4;
    let sequence_count = alphabet.len().pow(sequence_len as u32);

    for mut encoded in 0..sequence_count {
        let mut events = Vec::with_capacity(sequence_len);
        for ordinal in 0..sequence_len {
            let mut event = alphabet[encoded % alphabet.len()].clone();
            let start = (ordinal as u64) * 3 + 1;
            match &mut event {
                RolloutEvent::Message(message) => message.boundary = boundary(start),
                RolloutEvent::ToolCall(group) => {
                    group.start = boundary(start);
                    group.end = boundary(start + 1);
                }
                RolloutEvent::Compact { boundary: item, .. } => *item = boundary(start),
            }
            events.push(event);
            encoded /= alphabet.len();
        }

        let mut incremental = SpineReducer::new();
        for (index, event) in events.iter().enumerate() {
            incremental.apply(event.clone());
            assert_eq!(
                incremental.projection(),
                SpineReducer::derive(&events[..=index]),
                "bounded sequence {events:#?}, prefix {index}"
            );
        }
    }
}

#[test]
fn structural_node_ids_are_deterministic_under_replay() {
    let events = vec![
        open(1, "one"),
        next(3, "two", "one done"),
        open(5, "nested"),
    ];
    let first = SpineReducer::derive(&events);
    let second = SpineReducer::derive(&events);
    assert_eq!(first.nodes, second.nodes);
    assert_eq!(first.cursor.to_string(), "1.2.1");
}

#[test]
fn projection_last_boundary_tracks_native_event_boundary() {
    let projection = apply(&[message(4, MessageRole::User, "request"), open(8, "child")]);
    assert_eq!(projection.last_boundary, Some(boundary(9)));
}

#[test]
fn closed_node_projects_exactly_one_memory_item() {
    let projection = apply(&[open(1, "child"), close(3, "done")]);
    let (node_id, parts) = only_memory(&projection.visible_context[..1]);
    assert_eq!(node_id.to_string(), "1.1");
    assert_eq!(parts, &[MemoryPart::Model("done".to_string())]);
}
