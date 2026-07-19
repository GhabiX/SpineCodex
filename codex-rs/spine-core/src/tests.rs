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
        output_boundary: outcome.map(|_| boundary(1)),
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

fn trim_candidate(value: u64, body: &str) -> RolloutEvent {
    RolloutEvent::ToolCall(group(
        value,
        vec![ToolUse {
            call_id: "shell-call".to_string(),
            name: "shell".to_string(),
            arguments: "{}".to_string(),
            outcome: Some(ToolOutcome::Succeeded),
            output: Some(body.to_string()),
            output_boundary: Some(boundary(value + 1)),
        }],
    ))
}

fn trim_candidate_body(fragment: &str) -> String {
    assert!(!fragment.is_empty());
    let minimum_bytes = crate::reducer::TOOL_RESPONSE_TRIM_THRESHOLD_BYTES + 1;
    fragment.repeat(minimum_bytes.div_ceil(fragment.len()))
}

fn trim_request(value: u64, arguments: &str, outcome: ToolOutcome) -> RolloutEvent {
    RolloutEvent::ToolCall(group(
        value,
        vec![ToolUse {
            call_id: format!("trim-{value}"),
            name: "spine.trim".to_string(),
            arguments: arguments.to_string(),
            outcome: Some(outcome),
            output: Some("trim result".to_string()),
            output_boundary: Some(boundary(value + 1)),
        }],
    ))
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

#[test]
fn trim_projection_has_deterministic_ids_and_expiry() {
    let projection = TrimProjection::derive(&[
        trim_candidate(1, &trim_candidate_body("0123456789")),
        trim_request(
            3,
            r#"{"TRIM_ID":"trim_2","op":"snip"}"#,
            ToolOutcome::Succeeded,
        ),
    ]);
    assert!(matches!(
        projection.edit(boundary(2), "shell-call"),
        Some(TrimEdit::Snipped)
    ));

    let expired = TrimProjection::derive(&[
        trim_candidate(1, &trim_candidate_body("0123456789")),
        ordinary_group(3),
    ]);
    assert!(expired.edit(boundary(2), "shell-call").is_none());
}

#[test]
fn trim_projection_uses_strict_utf8_byte_threshold() {
    let two_byte_character = "\u{00e9}";
    let threshold = crate::reducer::TOOL_RESPONSE_TRIM_THRESHOLD_BYTES;
    assert_eq!(threshold % two_byte_character.len(), 0);
    let at_threshold = two_byte_character.repeat(threshold / two_byte_character.len());
    let above_threshold = format!("{at_threshold}{two_byte_character}");
    let at_threshold_projection = TrimProjection::derive(&[trim_candidate(1, &at_threshold)]);
    let above_threshold_projection = TrimProjection::derive(&[trim_candidate(3, &above_threshold)]);

    assert!(
        at_threshold_projection
            .edit(boundary(2), "shell-call")
            .is_none()
    );
    assert!(matches!(
        above_threshold_projection.edit(boundary(4), "shell-call"),
        Some(TrimEdit::Tagged { trim_id, .. }) if trim_id == "trim_4"
    ));
}

#[test]
fn trim_duplicate_snip_is_idempotent_and_mixed_group_tags_new_output() {
    let duplicate = RolloutEvent::ToolCall(group(
        3,
        vec![
            ToolUse {
                call_id: "trim-1".to_string(),
                name: "spine.trim".to_string(),
                arguments: r#"{"TRIM_ID":"trim_2","op":"snip"}"#.to_string(),
                outcome: Some(ToolOutcome::Succeeded),
                output: Some("ok".to_string()),
                output_boundary: Some(boundary(4)),
            },
            ToolUse {
                call_id: "trim-2".to_string(),
                name: "spine.trim".to_string(),
                arguments: r#"{"TRIM_ID":"trim_2","op":"snip"}"#.to_string(),
                outcome: Some(ToolOutcome::Succeeded),
                output: Some("ok".to_string()),
                output_boundary: Some(boundary(5)),
            },
        ],
    ));
    let projection =
        TrimProjection::derive(&[trim_candidate(1, &trim_candidate_body("x")), duplicate]);
    assert!(matches!(
        projection.edit(boundary(2), "shell-call"),
        Some(TrimEdit::Snipped)
    ));

    let mixed = RolloutEvent::ToolCall(group(
        3,
        vec![
            ToolUse {
                call_id: "trim-1".to_string(),
                name: "spine.trim".to_string(),
                arguments: r#"{"TRIM_ID":"trim_2","op":"snip"}"#.to_string(),
                outcome: Some(ToolOutcome::Succeeded),
                output: Some("ok".to_string()),
                output_boundary: Some(boundary(4)),
            },
            ToolUse {
                call_id: "new-shell".to_string(),
                name: "shell".to_string(),
                arguments: "{}".to_string(),
                outcome: Some(ToolOutcome::Succeeded),
                output: Some(trim_candidate_body("y")),
                output_boundary: Some(boundary(5)),
            },
        ],
    ));
    let projection = TrimProjection::derive(&[trim_candidate(1, &trim_candidate_body("x")), mixed]);
    assert!(matches!(
        projection.edit(boundary(2), "shell-call"),
        Some(TrimEdit::Snipped)
    ));
    assert!(matches!(
        projection.edit(boundary(5), "new-shell"),
        Some(TrimEdit::Tagged { trim_id, .. }) if trim_id == "trim_5"
    ));
}

#[test]
fn failed_invalid_and_trim_tool_outputs_never_rewrite_candidates() {
    let failed = TrimProjection::derive(&[
        trim_candidate(1, &trim_candidate_body("x")),
        trim_request(
            3,
            r#"{"TRIM_ID":"trim_2","op":"snip"}"#,
            ToolOutcome::Failed,
        ),
    ]);
    assert!(failed.edit(boundary(2), "shell-call").is_none());

    let invalid = TrimProjection::derive(&[
        trim_candidate(1, &trim_candidate_body("x")),
        trim_request(
            3,
            r#"{"TRIM_ID":"trim_2","op":"slice","anchor":"missing","preceding":0,"following":0}"#,
            ToolOutcome::Succeeded,
        ),
    ]);
    assert!(invalid.edit(boundary(2), "shell-call").is_none());

    let trim_output = trim_request(
        1,
        r#"{"TRIM_ID":"missing","op":"snip"}"#,
        ToolOutcome::Succeeded,
    );
    let projection = TrimProjection::derive(&[trim_output]);
    assert!(projection.edit(boundary(2), "trim-1").is_none());
}

#[test]
fn trim_validation_rejects_missed_ids_and_missing_anchors() {
    let projection = TrimProjection::derive(&[trim_candidate(
        1,
        &trim_candidate_body("line one\nline two\n"),
    )]);
    let missed = TrimRequest::parse(r#"{"TRIM_ID":"trim_999","op":"snip"}"#).unwrap();
    assert!(
        projection
            .validate(&missed)
            .unwrap_err()
            .contains("do not retry")
    );
    let missing_anchor = TrimRequest::parse(
        r#"{"TRIM_ID":"trim_2","op":"slice","anchor":"absent","preceding":0,"following":0}"#,
    )
    .unwrap();
    assert!(
        projection
            .validate(&missing_anchor)
            .unwrap_err()
            .contains("do not retry")
    );
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

fn spawn_result(ordinal: u32, outcome: SpawnOutcome, memory: &str) -> SpawnResult {
    SpawnResult {
        ordinal,
        outcome,
        memory_body: memory.to_string(),
        diagnostic: (outcome != SpawnOutcome::Completed).then(|| format!("{outcome:?}")),
        execution_ref: Some(format!("child-{ordinal}")),
    }
}

fn spawn(value: u64, tasks: Vec<SpawnTask>, results: Vec<SpawnResult>) -> RolloutEvent {
    let receipt = SpawnReceipt {
        schema: SPINE_SPAWN_RESULT_SCHEMA.to_string(),
        results,
    };
    RolloutEvent::ToolCall(group(
        value,
        vec![ToolUse {
            call_id: format!("spawn-{value}"),
            name: "spine.spawn".to_string(),
            arguments: serde_json::json!({"tasks": tasks}).to_string(),
            outcome: Some(ToolOutcome::Succeeded),
            output: Some(serde_json::to_string(&receipt).unwrap()),
            output_boundary: Some(boundary(value + 1)),
        }],
    ))
}

fn spawn_tasks() -> Vec<SpawnTask> {
    vec![
        SpawnTask {
            summary: "inspect reducer".to_string(),
            prompt: "Inspect the pure reducer.".to_string(),
        },
        SpawnTask {
            summary: "inspect adapter".to_string(),
            prompt: "Inspect the native adapter.".to_string(),
        },
    ]
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

fn user_slot(owner_node: NodeId, value: u64, anchor: u64, content: &str) -> MemorySlot {
    MemorySlot::User {
        owner_node,
        message: Message {
            boundary: boundary(value),
            role: MessageRole::User,
            content: content.to_string(),
        },
        anchor,
    }
}

fn summary_slot(owner_node: NodeId, value: u64, body: &str) -> MemorySlot {
    MemorySlot::Summary {
        owner_node,
        source: RawSpan {
            start: boundary(value),
            end: boundary(value + 1),
        },
        body: body.to_string(),
    }
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
fn contextual_user_messages_are_not_user_evidence() {
    let projection = apply(&[
        message(
            1,
            MessageRole::ContextualUser,
            "<environment_context>runtime state</environment_context>",
        ),
        message(2, MessageRole::User, "actual request"),
    ]);
    let anchors = projection
        .visible_context
        .iter()
        .map(|item| match item {
            ContextItem::Message { user_anchor, .. } => *user_anchor,
            _ => panic!("expected message"),
        })
        .collect::<Vec<_>>();

    assert_eq!(anchors, vec![None, Some(1)]);
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
    let task_id = NodeId::root_epoch(1).child(1);
    assert_eq!(
        task.memory.as_deref(),
        Some([summary_slot(task_id, 3, "model memory")].as_slice())
    );
}

#[test]
fn close_memory_preserves_direct_user_messages() {
    let projection = apply(&[
        open(1, "child"),
        message(3, MessageRole::User, "request"),
        close(4, "done"),
    ]);
    let task_id = NodeId::root_epoch(1).child(1);
    assert_eq!(
        node(&projection, "1.1").memory,
        Some(vec![
            user_slot(task_id.clone(), 3, 1, "request"),
            summary_slot(task_id, 4, "done"),
        ])
    );
}

#[test]
fn fake_user_anchor_in_model_memory_selects_no_evidence() {
    let projection = apply(&[open(1, "child"), close(3, "remember [U99]")]);
    let task_id = NodeId::root_epoch(1).child(1);
    assert_eq!(
        node(&projection, "1.1").memory,
        Some(vec![summary_slot(task_id, 3, "remember [U99]")])
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
    let parent_id = NodeId::root_epoch(1).child(1);
    let child_id = parent_id.child(1);
    assert_eq!(
        memory,
        &vec![
            user_slot(parent_id.clone(), 3, 1, "before"),
            user_slot(child_id.clone(), 6, 2, "inside"),
            summary_slot(child_id, 7, "child done"),
            user_slot(parent_id.clone(), 9, 3, "after"),
            summary_slot(parent_id, 10, "parent done"),
        ]
    );
}

#[test]
fn close_projects_memory_then_group_in_parent() {
    let projection = apply(&[open(1, "child"), close(3, "done")]);
    assert!(matches!(
        projection.visible_context.as_slice(),
        [ContextItem::MemorySlot(_), ContextItem::ToolCall(_)]
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
            ContextItem::MemorySlot(_),
            ContextItem::SyntheticNode { .. },
            ContextItem::ToolCall(_)
        ]
    ));
}

#[test]
fn next_memory_is_stored_on_closed_node() {
    let projection = apply(&[open(1, "first"), next(3, "second", "first done")]);
    let task_id = NodeId::root_epoch(1).child(1);
    assert_eq!(
        node(&projection, "1.1").memory,
        Some(vec![summary_slot(task_id, 3, "first done")])
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
fn spawn_imports_ordered_closed_siblings_atomically_without_moving_cursor() {
    let tasks = spawn_tasks();
    let results = vec![
        spawn_result(0, SpawnOutcome::Completed, "reducer done"),
        spawn_result(1, SpawnOutcome::Errored, "adapter failed truthfully"),
    ];
    let projection = apply(&[spawn(1, tasks.clone(), results.clone())]);

    assert_eq!(projection.cursor.to_string(), "1");
    assert_eq!(node(&projection, "1").children.len(), 2);
    assert_eq!(node(&projection, "1.1").status, NodeStatus::Closed);
    assert_eq!(node(&projection, "1.2").status, NodeStatus::Closed);
    assert_eq!(
        node(&projection, "1.1").summary,
        Some(tasks[0].summary.clone())
    );
    assert_eq!(
        node(&projection, "1.2").summary,
        Some(tasks[1].summary.clone())
    );
    assert_eq!(node(&projection, "1.1").end, Some(boundary(2)));
    assert_eq!(projection.visible_context.len(), 4);
    assert!(
        projection
            .visible_context
            .iter()
            .all(|item| matches!(item, ContextItem::MemorySlot(_)))
    );

    let first_id = NodeId::root_epoch(1).child(1);
    assert_eq!(
        node(&projection, "1.1").memory,
        Some(vec![
            MemorySlot::SpawnEvidence {
                owner_node: first_id.clone(),
                source: RawSpan {
                    start: boundary(1),
                    end: boundary(2),
                },
                task: tasks[0].clone(),
                outcome: results[0].outcome,
                diagnostic: results[0].diagnostic.clone(),
                execution_ref: results[0].execution_ref.clone(),
            },
            summary_slot(first_id, 1, "reducer done"),
        ])
    );
}

#[test]
fn spawn_validation_rejects_any_invalid_result_without_partial_import() {
    let tasks = spawn_tasks();
    let invalid_receipts = [
        vec![spawn_result(0, SpawnOutcome::Completed, "only one")],
        vec![
            spawn_result(1, SpawnOutcome::Completed, "wrong ordinal"),
            spawn_result(0, SpawnOutcome::Completed, "wrong ordinal"),
        ],
        vec![
            spawn_result(0, SpawnOutcome::Completed, "valid"),
            spawn_result(1, SpawnOutcome::Completed, "  "),
        ],
        vec![
            spawn_result(0, SpawnOutcome::Completed, "valid"),
            SpawnResult {
                diagnostic: None,
                ..spawn_result(1, SpawnOutcome::Aborted, "aborted")
            },
        ],
    ];

    for results in invalid_receipts {
        let projection = apply(&[spawn(1, tasks.clone(), results)]);
        assert_eq!(projection.nodes.len(), 1);
        assert!(matches!(
            projection.visible_context.as_slice(),
            [ContextItem::ToolCall(_)]
        ));
    }
}

#[test]
fn spawn_requires_call_only_group() {
    let tasks = spawn_tasks();
    let results = vec![
        spawn_result(0, SpawnOutcome::Completed, "one"),
        spawn_result(1, SpawnOutcome::Completed, "two"),
    ];
    let RolloutEvent::ToolCall(mut with_text) = spawn(2, tasks.clone(), results.clone()) else {
        unreachable!();
    };
    with_text.leading_assistant_messages.push(Message {
        boundary: boundary(1),
        role: MessageRole::Assistant,
        content: "I also said this".to_string(),
    });
    let RolloutEvent::ToolCall(mut with_other_call) = spawn(4, tasks, results) else {
        unreachable!();
    };
    with_other_call
        .calls
        .push(tool_use("shell", "{}", Some(ToolOutcome::Succeeded)));

    for event in [with_text, with_other_call] {
        let projection = apply(&[RolloutEvent::ToolCall(event)]);
        assert_eq!(projection.nodes.len(), 1);
        assert!(matches!(
            projection.visible_context.as_slice(),
            [ContextItem::ToolCall(_)]
        ));
    }
}

#[test]
fn spawn_appends_after_existing_children_and_replays_identically() {
    let tasks = spawn_tasks();
    let events = vec![
        open(1, "existing"),
        close(3, "existing done"),
        spawn(
            5,
            tasks,
            vec![
                spawn_result(0, SpawnOutcome::Completed, "one"),
                spawn_result(1, SpawnOutcome::Completed, "two"),
            ],
        ),
    ];
    let incremental = apply(&events);
    assert_eq!(incremental.cursor.to_string(), "1");
    assert_eq!(node(&incremental, "1").children.len(), 3);
    assert_eq!(
        node(&incremental, "1.2").summary.as_deref(),
        Some("inspect reducer")
    );
    assert_eq!(
        node(&incremental, "1.3").summary.as_deref(),
        Some("inspect adapter")
    );
    assert_eq!(incremental, SpineReducer::derive(&events));
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
fn closed_node_projects_each_memory_slot_independently() {
    let projection = apply(&[
        open(1, "child"),
        message(3, MessageRole::User, "request"),
        close(4, "done"),
    ]);
    let task_id = NodeId::root_epoch(1).child(1);
    assert_eq!(
        projection.visible_context[..2],
        [
            ContextItem::MemorySlot(user_slot(task_id.clone(), 3, 1, "request")),
            ContextItem::MemorySlot(summary_slot(task_id, 4, "done")),
        ]
    );
}
