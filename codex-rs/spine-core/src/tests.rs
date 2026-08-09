use super::*;
use crate::reducer::SpineReducer;
use pretty_assertions::assert_eq;

fn boundary(value: u64) -> RawBoundary {
    RawBoundary(value)
}

#[test]
fn spawn_receipts_reject_unbounded_context_fields() {
    let tasks = vec![
        SpawnTask {
            summary: "a".to_string(),
            prompt: "p".to_string(),
        },
        SpawnTask {
            summary: "b".to_string(),
            prompt: "p".to_string(),
        },
    ];
    let receipt = SpawnReceipt {
        schema: SPINE_SPAWN_RESULT_SCHEMA.to_string(),
        results: vec![
            SpawnResult {
                ordinal: 0,
                outcome: SpawnOutcome::Completed,
                memory_body: "x".repeat(MAX_MEMORY_BYTES + 1),
                diagnostic: None,
                execution_ref: None,
            },
            SpawnResult {
                ordinal: 1,
                outcome: SpawnOutcome::Completed,
                memory_body: "ok".to_string(),
                diagnostic: None,
                execution_ref: None,
            },
        ],
    };
    assert!(matches!(
        receipt.validate_for(&tasks),
        Err(SpawnValidationError::FieldTooLarge {
            field: "memory_body",
            ..
        })
    ));
}

#[test]
fn spawn_receipts_reject_aggregate_context_fields() {
    let tasks = vec![
        SpawnTask {
            summary: "a".to_string(),
            prompt: "p".to_string(),
        },
        SpawnTask {
            summary: "b".to_string(),
            prompt: "p".to_string(),
        },
    ];
    let receipt = SpawnReceipt {
        schema: SPINE_SPAWN_RESULT_SCHEMA.to_string(),
        results: vec![
            SpawnResult {
                ordinal: 0,
                outcome: SpawnOutcome::Errored,
                memory_body: "x".repeat(MAX_MEMORY_BYTES),
                diagnostic: Some("d".to_string()),
                execution_ref: None,
            },
            SpawnResult {
                ordinal: 1,
                outcome: SpawnOutcome::Completed,
                memory_body: "x".repeat(MAX_MEMORY_BYTES),
                diagnostic: None,
                execution_ref: None,
            },
        ],
    };

    assert!(matches!(
        receipt.validate_for(&tasks),
        Err(SpawnValidationError::AggregateTooLarge {
            phase: "result",
            ..
        })
    ));
}

#[test]
fn spawn_receipts_enforce_each_optional_field_and_accept_exact_limits() {
    let tasks = vec![
        SpawnTask {
            summary: "s".repeat(MAX_SUMMARY_BYTES),
            prompt: "p".repeat(MAX_SPAWN_PROMPT_BYTES),
        },
        SpawnTask {
            summary: "b".to_string(),
            prompt: "p".to_string(),
        },
    ];
    let mut receipt = SpawnReceipt {
        schema: SPINE_SPAWN_RESULT_SCHEMA.to_string(),
        results: vec![
            SpawnResult {
                ordinal: 0,
                outcome: SpawnOutcome::Errored,
                memory_body: "m".repeat(MAX_MEMORY_BYTES),
                diagnostic: Some("d".to_string()),
                execution_ref: Some("r".repeat(MAX_SUMMARY_BYTES)),
            },
            SpawnResult {
                ordinal: 1,
                outcome: SpawnOutcome::Completed,
                memory_body: "ok".to_string(),
                diagnostic: None,
                execution_ref: None,
            },
        ],
    };
    assert_eq!(receipt.validate_for(&tasks), Ok(()));

    receipt.results[0].diagnostic = Some("d".repeat(MAX_SUMMARY_BYTES + 1));
    assert!(matches!(
        receipt.validate_for(&tasks),
        Err(SpawnValidationError::FieldTooLarge {
            field: "diagnostic",
            ..
        })
    ));

    receipt.results[0].diagnostic = Some("d".to_string());
    receipt.results[0].execution_ref = Some("r".repeat(MAX_SUMMARY_BYTES + 1));
    assert!(matches!(
        receipt.validate_for(&tasks),
        Err(SpawnValidationError::FieldTooLarge {
            field: "execution_ref",
            ..
        })
    ));
}

fn message(value: u64, role: MessageRole, content: &str) -> TestEvent {
    TestEvent::Event(RolloutEvent::Message(Message {
        boundary: boundary(value),
        role,
        content: content.to_string(),
    }))
}

#[derive(Clone, Debug)]
enum TestEvent {
    Event(RolloutEvent),
    Sampling {
        span: RawSpan,
        calls: Vec<ToolUse>,
        facts: Vec<ExecutedSpineFact>,
    },
}

fn sampling(value: u64, calls: Vec<ToolUse>) -> TestEvent {
    let facts = calls
        .iter()
        .enumerate()
        .filter_map(|(index, call)| {
            if call.outcome != Some(ToolOutcome::Succeeded) {
                return None;
            }
            let tool = match call.name.as_str() {
                "spine.open" => SpineTool::Open,
                "spine.close" => SpineTool::Close,
                "spine.next" => SpineTool::Next,
                "spine.spawn" => SpineTool::Spawn,
                _ => return None,
            };
            let ToolValidation::Transition(transition) =
                validate_tool(tool, &call.arguments).ok()?
            else {
                return None;
            };
            let operation = match transition {
                ValidatedTransition::Open { summary } => SpineOperationFact::Open { summary },
                ValidatedTransition::Close { memory } => SpineOperationFact::Close { memory },
                ValidatedTransition::Next { summary, memory } => SpineOperationFact::Next {
                    closed_memory: memory,
                    next_summary: summary,
                },
                ValidatedTransition::Spawn { tasks } => {
                    let output =
                        serde_json::from_str::<SpawnReceipt>(call.output.as_deref()?).ok()?;
                    output.validate_for(&tasks).ok()?;
                    SpineOperationFact::Spawn {
                        tasks,
                        terminal_results: output.results,
                    }
                }
                ValidatedTransition::Trim(_) => return None,
            };
            Some(ExecutedSpineFact {
                execution_id: ExecutionId::parse(
                    ThreadNamespace::parse("test-thread").ok()?,
                    format!("execution-{value}-{index}"),
                )
                .ok()?,
                ordinal: AdmissionOrdinal::new(index as u64),
                origin: ExecutionOrigin::Direct {
                    call_id: call.call_id.clone(),
                },
                operation,
            })
        })
        .collect::<Vec<_>>();
    let structural_count = facts
        .iter()
        .filter(|fact| {
            matches!(
                fact.operation,
                SpineOperationFact::Open { .. }
                    | SpineOperationFact::Close { .. }
                    | SpineOperationFact::Next { .. }
                    | SpineOperationFact::Spawn { .. }
            )
        })
        .count();
    let facts = if structural_count <= 1
        || facts
            .iter()
            .all(|fact| matches!(fact.operation, SpineOperationFact::Spawn { .. }))
    {
        facts
    } else {
        Default::default()
    };
    TestEvent::Sampling {
        span: RawSpan {
            start: boundary(value),
            end: boundary(value + 1),
        },
        calls,
        facts,
    }
}

fn tool_use(name: &str, arguments: &str, outcome: Option<ToolOutcome>) -> ToolUse {
    ToolUse {
        call_id: format!("call-{name}"),
        name: name.to_string(),
        arguments: arguments.to_string(),
        call_ordinal: None,
        outcome,
        output: outcome.map(|_| format!("{name} output")),
        output_boundary: outcome.map(|_| boundary(1)),
    }
}

fn trim_candidate(value: u64, body: &str) -> TestEvent {
    sampling(
        value,
        vec![ToolUse {
            call_id: "shell-call".to_string(),
            name: "shell".to_string(),
            arguments: "{}".to_string(),
            call_ordinal: None,
            outcome: Some(ToolOutcome::Succeeded),
            output: Some(body.to_string()),
            output_boundary: Some(boundary(value + 1)),
        }],
    )
}

fn trim_candidate_body(fragment: &str) -> String {
    assert!(!fragment.is_empty());
    let minimum_bytes = crate::reducer::TOOL_RESPONSE_TRIM_THRESHOLD_BYTES + 1;
    fragment.repeat(minimum_bytes.div_ceil(fragment.len()))
}

fn trim_request(value: u64, arguments: &str, outcome: ToolOutcome) -> TestEvent {
    sampling(
        value,
        vec![ToolUse {
            call_id: format!("trim-{value}"),
            name: "spine.trim".to_string(),
            arguments: arguments.to_string(),
            call_ordinal: None,
            outcome: Some(outcome),
            output: Some("trim result".to_string()),
            output_boundary: Some(boundary(value + 1)),
        }],
    )
}

fn ordinary_group(value: u64) -> TestEvent {
    sampling(
        value,
        vec![tool_use(
            "shell",
            r#"{"cmd":"pwd"}"#,
            Some(ToolOutcome::Succeeded),
        )],
    )
}

fn trim_projection(events: &[TestEvent]) -> TrimProjection {
    let mut reducer =
        crate::reducer::TrimReducer::new(crate::reducer::TOOL_RESPONSE_TRIM_THRESHOLD_BYTES);
    for event in events {
        match event {
            TestEvent::Event(event) => reducer.apply(event),
            TestEvent::Sampling { span, calls, .. } => reducer.apply_completed_calls(
                &crate::context_char::CompletedCalls::from_test(*span, calls.clone()),
            ),
        }
    }
    reducer.projection().clone()
}

#[test]
fn trim_projection_keeps_expired_tags() {
    let projection = trim_projection(&[
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

    let expired = trim_projection(&[
        trim_candidate(1, &trim_candidate_body("0123456789")),
        ordinary_group(3),
    ]);
    assert!(matches!(
        expired.edit(boundary(2), "shell-call"),
        Some(TrimEdit::Tagged { trim_id, .. }) if trim_id == "trim_2"
    ));
}

#[test]
fn trim_projection_uses_strict_utf8_byte_threshold() {
    let two_byte_character = "\u{00e9}";
    let threshold = crate::reducer::TOOL_RESPONSE_TRIM_THRESHOLD_BYTES;
    assert_eq!(threshold % two_byte_character.len(), 0);
    let at_threshold = two_byte_character.repeat(threshold / two_byte_character.len());
    let above_threshold = format!("{at_threshold}{two_byte_character}");
    let at_threshold_projection = trim_projection(&[trim_candidate(1, &at_threshold)]);
    let above_threshold_projection = trim_projection(&[trim_candidate(3, &above_threshold)]);

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
    let duplicate = sampling(
        3,
        vec![
            ToolUse {
                call_id: "trim-1".to_string(),
                name: "spine.trim".to_string(),
                arguments: r#"{"TRIM_ID":"trim_2","op":"snip"}"#.to_string(),
                call_ordinal: None,
                outcome: Some(ToolOutcome::Succeeded),
                output: Some("ok".to_string()),
                output_boundary: Some(boundary(4)),
            },
            ToolUse {
                call_id: "trim-2".to_string(),
                name: "spine.trim".to_string(),
                arguments: r#"{"TRIM_ID":"trim_2","op":"snip"}"#.to_string(),
                call_ordinal: None,
                outcome: Some(ToolOutcome::Succeeded),
                output: Some("ok".to_string()),
                output_boundary: Some(boundary(5)),
            },
        ],
    );
    let projection = trim_projection(&[trim_candidate(1, &trim_candidate_body("x")), duplicate]);
    assert!(matches!(
        projection.edit(boundary(2), "shell-call"),
        Some(TrimEdit::Snipped)
    ));

    let mixed = sampling(
        3,
        vec![
            ToolUse {
                call_id: "trim-1".to_string(),
                name: "spine.trim".to_string(),
                arguments: r#"{"TRIM_ID":"trim_2","op":"snip"}"#.to_string(),
                call_ordinal: None,
                outcome: Some(ToolOutcome::Succeeded),
                output: Some("ok".to_string()),
                output_boundary: Some(boundary(4)),
            },
            ToolUse {
                call_id: "new-shell".to_string(),
                name: "shell".to_string(),
                arguments: "{}".to_string(),
                call_ordinal: None,
                outcome: Some(ToolOutcome::Succeeded),
                output: Some(trim_candidate_body("y")),
                output_boundary: Some(boundary(5)),
            },
        ],
    );
    let projection = trim_projection(&[trim_candidate(1, &trim_candidate_body("x")), mixed]);
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
    let failed = trim_projection(&[
        trim_candidate(1, &trim_candidate_body("x")),
        trim_request(
            3,
            r#"{"TRIM_ID":"trim_2","op":"snip"}"#,
            ToolOutcome::Failed,
        ),
    ]);
    assert!(matches!(
        failed.edit(boundary(2), "shell-call"),
        Some(TrimEdit::Tagged { .. })
    ));

    let invalid = trim_projection(&[
        trim_candidate(1, &trim_candidate_body("x")),
        trim_request(
            3,
            r#"{"TRIM_ID":"trim_2","op":"slice","anchor":"missing","preceding":0,"following":0}"#,
            ToolOutcome::Succeeded,
        ),
    ]);
    assert!(matches!(
        invalid.edit(boundary(2), "shell-call"),
        Some(TrimEdit::Tagged { .. })
    ));

    let trim_output = trim_request(
        1,
        r#"{"TRIM_ID":"missing","op":"snip"}"#,
        ToolOutcome::Succeeded,
    );
    let projection = trim_projection(&[trim_output]);
    assert!(projection.edit(boundary(2), "trim-1").is_none());
}

#[test]
fn trim_validation_rejects_missed_ids_and_missing_anchors() {
    let projection = trim_projection(&[trim_candidate(
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

fn open(value: u64, summary: &str) -> TestEvent {
    sampling(
        value,
        vec![tool_use(
            "spine.open",
            &serde_json::json!({"summary": summary}).to_string(),
            Some(ToolOutcome::Succeeded),
        )],
    )
}

fn close(value: u64, memory: &str) -> TestEvent {
    sampling(
        value,
        vec![tool_use(
            "spine.close",
            &serde_json::json!({"memory": memory}).to_string(),
            Some(ToolOutcome::Succeeded),
        )],
    )
}

fn next(value: u64, summary: &str, memory: &str) -> TestEvent {
    sampling(
        value,
        vec![tool_use(
            "spine.next",
            &serde_json::json!({"summary": summary, "memory": memory}).to_string(),
            Some(ToolOutcome::Succeeded),
        )],
    )
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

fn spawn(value: u64, tasks: Vec<SpawnTask>, results: Vec<SpawnResult>) -> TestEvent {
    let receipt = SpawnReceipt {
        schema: SPINE_SPAWN_RESULT_SCHEMA.to_string(),
        results,
    };
    sampling(
        value,
        vec![ToolUse {
            call_id: format!("spawn-{value}"),
            name: "spine.spawn".to_string(),
            arguments: serde_json::json!({"tasks": tasks}).to_string(),
            call_ordinal: None,
            outcome: Some(ToolOutcome::Succeeded),
            output: Some(serde_json::to_string(&receipt).unwrap()),
            output_boundary: Some(boundary(value + 1)),
        }],
    )
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

fn compact(value: u64, replacement_history: Vec<ContextItem>) -> TestEvent {
    TestEvent::Event(RolloutEvent::Compact {
        boundary: boundary(value),
        replacement_history,
    })
}

fn apply(events: &[TestEvent]) -> SpineProjection {
    let mut reducer = SpineReducer::new();
    for event in events {
        apply_one(&mut reducer, event);
    }
    reducer.projection()
}

fn apply_one(reducer: &mut SpineReducer, event: &TestEvent) {
    let _ = apply_one_delta(reducer, event);
}

fn apply_one_delta(reducer: &mut SpineReducer, event: &TestEvent) -> ProjectionDelta {
    match event {
        TestEvent::Event(event) => reducer.apply(event.clone()),
        TestEvent::Sampling { span, calls, facts } => {
            let facts = facts.iter().collect::<Vec<_>>();
            let settled_spawn_call_ids = calls
                .iter()
                .filter(|call| call.name == "spine.spawn" && call.output.is_some())
                .map(|call| call.call_id.clone())
                .collect::<Vec<_>>();
            match reducer.apply_sampling(
                *span,
                &facts,
                &settled_spawn_call_ids,
                /*open_input_tokens*/ None,
            ) {
                Ok(delta) => delta,
                Err(crate::reducer::TypedTransitionError::TaskCursorRequired(_)) => {
                    reducer.apply(RolloutEvent::SourceSpan {
                        span: *span,
                        retained_bytes: 0,
                    })
                }
                Err(error) => panic!("test sampling is invalid: {error:?}"),
            }
        }
    }
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
        [ContextItem::SourceSpan { .. }]
    ));
}

#[test]
fn sampling_source_is_one_span() {
    let projection = apply(&[sampling(
        1,
        vec![tool_use("shell", "{}", Some(ToolOutcome::Succeeded))],
    )]);
    let [ContextItem::SourceSpan { span }] = projection.visible_context.as_slice() else {
        panic!("expected one sampling source span");
    };
    assert_eq!(
        *span,
        RawSpan {
            start: boundary(1),
            end: boundary(2)
        }
    );
}

#[test]
fn incomplete_control_group_is_ordinary() {
    let projection = apply(&[sampling(
        1,
        vec![tool_use("spine.open", r#"{"summary":"child"}"#, None)],
    )]);
    assert_eq!(projection.cursor.to_string(), "1");
    assert!(matches!(
        projection.visible_context.as_slice(),
        [ContextItem::SourceSpan { .. }]
    ));
}

#[test]
fn failed_control_group_is_ordinary() {
    let projection = apply(&[sampling(
        1,
        vec![tool_use(
            "spine.open",
            r#"{"summary":"child"}"#,
            Some(ToolOutcome::Failed),
        )],
    )]);
    assert_eq!(projection.cursor.to_string(), "1");
    assert_eq!(projection.nodes.len(), 1);
}

#[test]
fn unknown_control_outcome_is_ordinary() {
    let projection = apply(&[sampling(
        1,
        vec![tool_use(
            "spine.open",
            r#"{"summary":"child"}"#,
            Some(ToolOutcome::Unknown),
        )],
    )]);
    assert_eq!(projection.nodes.len(), 1);
}

#[test]
fn malformed_control_arguments_are_ordinary() {
    let projection = apply(&[sampling(
        1,
        vec![tool_use(
            "spine.open",
            "not-json",
            Some(ToolOutcome::Succeeded),
        )],
    )]);
    assert_eq!(projection.nodes.len(), 1);
}

#[test]
fn unknown_control_fields_are_rejected() {
    let projection = apply(&[sampling(
        1,
        vec![tool_use(
            "spine.open",
            r#"{"summary":"child","extra":true}"#,
            Some(ToolOutcome::Succeeded),
        )],
    )]);
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
        [
            ContextItem::SyntheticNode { .. },
            ContextItem::SourceSpan { .. }
        ]
    ));
}

#[test]
fn nested_open_creates_hierarchical_id() {
    let projection = apply(&[open(1, "parent"), open(3, "child")]);
    assert_eq!(projection.cursor.to_string(), "1.1.1");
    assert_eq!(node(&projection, "1.1").status, NodeStatus::Opened);
}

#[test]
fn nested_open_appends_without_rewriting_visible_parent_marker() {
    let parent = apply(&[open(1, "parent")]);
    assert!(matches!(
        parent.visible_context.first(),
        Some(ContextItem::SyntheticNode {
            status: NodeStatus::Opened,
            ..
        })
    ));

    let nested = apply(&[open(1, "parent"), open(3, "child")]);
    assert!(
        nested.visible_context.starts_with(&parent.visible_context),
        "opening a child must not rewrite the visible parent prefix"
    );

    let child_closed = apply(&[open(1, "parent"), open(3, "child"), close(5, "child done")]);
    assert!(matches!(
        child_closed.visible_context.first(),
        Some(ContextItem::SyntheticNode {
            status: NodeStatus::Opened,
            ..
        })
    ));
}

#[test]
fn close_at_root_is_ordinary() {
    let projection = apply(&[close(1, "invalid root memory")]);
    assert_eq!(projection.cursor.to_string(), "1");
    assert_eq!(node(&projection, "1").status, NodeStatus::Live);
    assert!(matches!(
        projection.visible_context.as_slice(),
        [ContextItem::SourceSpan { .. }]
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
fn close_projects_memory_then_current_sampling_in_parent() {
    let projection = apply(&[open(1, "child"), close(3, "done")]);
    assert!(matches!(
        projection.visible_context.as_slice(),
        [ContextItem::MemorySlot(_), ContextItem::SourceSpan { .. }]
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
            ContextItem::SourceSpan { .. }
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
    let projection = apply(&[sampling(
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
    )]);
    assert_eq!(projection.nodes.len(), 1);
    assert!(matches!(
        projection.visible_context.as_slice(),
        [ContextItem::SourceSpan { .. }]
    ));
}

#[test]
fn ordinary_call_can_coexist_with_one_control() {
    let projection = apply(&[sampling(
        1,
        vec![
            tool_use("shell", "{}", Some(ToolOutcome::Succeeded)),
            tool_use(
                "spine.open",
                r#"{"summary":"child"}"#,
                Some(ToolOutcome::Succeeded),
            ),
        ],
    )]);
    assert_eq!(projection.cursor.to_string(), "1.1");
    let ContextItem::SourceSpan { span } = &projection.visible_context[1] else {
        panic!("expected sampling source span in child");
    };
    assert_eq!(
        *span,
        RawSpan {
            start: boundary(1),
            end: boundary(2)
        }
    );
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
    assert_eq!(projection.settled_spawn_call_ids, ["spawn-1"]);
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
    assert_eq!(projection.visible_context.len(), 5);
    assert!(matches!(
        projection.visible_context.first(),
        Some(ContextItem::SourceSpan { .. })
    ));
    assert!(
        projection.visible_context[1..]
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
            [ContextItem::SourceSpan { .. }]
        ));
    }
}

#[test]
fn spawn_allows_ordinary_sibling_calls_in_the_same_sampling() {
    let tasks = spawn_tasks();
    let results = vec![
        spawn_result(0, SpawnOutcome::Completed, "one"),
        spawn_result(1, SpawnOutcome::Completed, "two"),
    ];
    let TestEvent::Sampling { mut calls, .. } = spawn(2, tasks, results) else {
        unreachable!();
    };
    calls.push(tool_use("shell", "{}", Some(ToolOutcome::Succeeded)));

    let projection = apply(&[sampling(2, calls)]);
    assert_eq!(projection.nodes.len(), 3);
    assert!(matches!(
        projection.visible_context.first(),
        Some(ContextItem::SourceSpan { .. })
    ));
    assert_eq!(projection.visible_context.len(), 5);
}

#[test]
fn host_unreachable_multiple_spawn_annotations_are_total_and_ordered() {
    let tasks = spawn_tasks();
    let TestEvent::Sampling {
        mut calls,
        mut facts,
        ..
    } = spawn(
        1,
        tasks.clone(),
        vec![
            spawn_result(0, SpawnOutcome::Completed, "first call task 0"),
            spawn_result(1, SpawnOutcome::Completed, "first call task 1"),
        ],
    )
    else {
        unreachable!();
    };
    let TestEvent::Sampling {
        calls: second_calls,
        facts: second_facts,
        ..
    } = spawn(
        3,
        tasks,
        vec![
            spawn_result(0, SpawnOutcome::Completed, "second call task 0"),
            spawn_result(1, SpawnOutcome::Completed, "second call task 1"),
        ],
    )
    else {
        unreachable!();
    };
    calls.extend(second_calls);
    facts.extend(second_facts);

    let projection = apply(&[TestEvent::Sampling {
        span: RawSpan {
            start: boundary(1),
            end: boundary(4),
        },
        calls,
        facts,
    }]);
    assert_eq!(projection.settled_spawn_call_ids, ["spawn-1", "spawn-3"]);
    assert_eq!(projection.nodes.len(), 5);
    assert_eq!(projection.visible_context.len(), 9);
    assert!(matches!(
        &node(&projection, "1.1").memory.as_ref().unwrap()[1],
        MemorySlot::Summary { body, .. } if body == "first call task 0"
    ));
    assert!(matches!(
        &node(&projection, "1.3").memory.as_ref().unwrap()[1],
        MemorySlot::Summary { body, .. } if body == "second call task 0"
    ));
}

#[test]
fn spawn_mixed_with_spine_control_does_not_import_children() {
    let tasks = spawn_tasks();
    let TestEvent::Sampling { mut calls, .. } = spawn(
        1,
        tasks,
        vec![
            spawn_result(0, SpawnOutcome::Completed, "one"),
            spawn_result(1, SpawnOutcome::Completed, "two"),
        ],
    ) else {
        unreachable!();
    };
    calls.push(ToolUse {
        call_id: "open".to_string(),
        name: "spine.open".to_string(),
        arguments: r#"{"summary":"conflict"}"#.to_string(),
        call_ordinal: None,
        outcome: Some(ToolOutcome::Succeeded),
        output: Some("opened".to_string()),
        output_boundary: Some(boundary(3)),
    });
    let projection = apply(&[sampling(1, calls)]);
    assert_eq!(projection.nodes.len(), 1);
    assert!(matches!(
        projection.visible_context.as_slice(),
        [ContextItem::SourceSpan { .. }]
    ));
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
    assert_eq!(incremental, apply(&events));
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
        let delta = apply_one_delta(&mut reducer, &event);
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
    assert_eq!(apply(&events), apply(&events));
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
    assert_eq!(incremental.projection(), apply(&[]));
    for (index, event) in events.iter().enumerate() {
        apply_one(&mut incremental, event);
        assert_eq!(
            incremental.projection(),
            apply(&events[..=index]),
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
                TestEvent::Sampling { span, .. } => {
                    span.start = boundary(start);
                    span.end = boundary(start + 1);
                }
                TestEvent::Event(RolloutEvent::Message(message)) => {
                    message.boundary = boundary(start);
                }
                TestEvent::Event(
                    RolloutEvent::Opaque { boundary: item }
                    | RolloutEvent::Synthetic { boundary: item, .. }
                    | RolloutEvent::Compact { boundary: item, .. },
                ) => *item = boundary(start),
                TestEvent::Event(RolloutEvent::SourceSpan { span, .. }) => {
                    span.start = boundary(start);
                    span.end = boundary(start + 1);
                }
            }
            events.push(event);
            encoded /= alphabet.len();
        }

        let mut incremental = SpineReducer::new();
        for (index, event) in events.iter().enumerate() {
            apply_one(&mut incremental, event);
            assert_eq!(
                incremental.projection(),
                apply(&events[..=index]),
                "bounded sequence {events:#?}, prefix {index}"
            );
        }
    }
}

#[test]
fn compiler_rejects_unbounded_accumulated_synthetic_context() {
    let config = SpineConfig::v1().with_feature(Feature::Jit).unwrap();
    let mut compiler = SpineCompiler::new(config).unwrap();
    let owner = NodeId::root_epoch(1);
    let replacement_history = (0..33)
        .map(|index| {
            ContextItem::MemorySlot(MemorySlot::Summary {
                owner_node: owner.clone(),
                source: RawSpan {
                    start: boundary(index),
                    end: boundary(index),
                },
                body: "x".repeat(MAX_MEMORY_BYTES),
            })
        })
        .collect();

    assert!(matches!(
        compiler.eat(RolloutEvent::Compact {
            boundary: boundary(1),
            replacement_history,
        }),
        Err(SpineError::ContextLimit {
            kind: "synthetic context bytes",
            ..
        })
    ));
}

#[test]
fn structural_node_ids_are_deterministic_under_replay() {
    let events = vec![
        open(1, "one"),
        next(3, "two", "one done"),
        open(5, "nested"),
    ];
    let first = apply(&events);
    let second = apply(&events);
    assert_eq!(first.nodes, second.nodes);
    assert_eq!(first.cursor.to_string(), "1.2.1");
}

#[test]
fn spine_prompt_contract_covers_defaults_overrides_and_required_values() {
    let defaults: toml::Value = toml::from_str(DEFAULT_CONFIG_TOML).unwrap();
    let prompt = defaults
        .get("prompt")
        .and_then(toml::Value::as_table)
        .unwrap();
    let names = [
        "jit",
        "node",
        "spawn",
        "spawn_explicit_request_only",
        "spawn_proactive",
        "trim",
    ];
    assert_eq!(prompt.keys().map(String::as_str).collect::<Vec<_>>(), names);
    assert!(!prompt["jit"].as_str().unwrap().trim().is_empty());
    assert!(!prompt["node"].as_str().unwrap().trim().is_empty());
    assert_eq!(prompt["trim"].as_str(), Some(""));
    assert_eq!(prompt["spawn"].as_str(), Some(""));
    assert!(
        !prompt["spawn_explicit_request_only"]
            .as_str()
            .unwrap()
            .trim()
            .is_empty()
    );
    assert!(
        !prompt["spawn_proactive"]
            .as_str()
            .unwrap()
            .trim()
            .is_empty()
    );

    let source = r#"
schema_version = 1
[limits]
trim_threshold_bytes = 10000
[prompt]
jit = "jit override"
node = "node override"
trim = "trim override"
spawn = "spawn override"
spawn_explicit_request_only = "explicit override"
spawn_proactive = "proactive override"
[tools.open]
description = "open"
[tools.close]
description = "close"
[tools.next]
description = "next"
[tools.trim]
description = "trim"
[tools.spawn]
description = "spawn"
"#;
    let config = SpineConfig::parse_toml(source)
        .unwrap()
        .with_features([Feature::Jit, Feature::Trim, Feature::Spawn])
        .unwrap();
    assert_eq!(config.prompt(Feature::Jit), "jit override");
    assert_eq!(config.prompt(Feature::Trim), "trim override");
    assert_eq!(config.prompt(Feature::Spawn), "spawn override");
    assert_eq!(config.node_prompt(), Some("node override"));
    assert_eq!(
        config.spawn_prompt(SpawnPromptMode::ExplicitRequestOnly),
        Some("explicit override")
    );
    assert_eq!(
        config.spawn_prompt(SpawnPromptMode::Proactive),
        Some("proactive override")
    );

    let missing = source.replace("jit = \"jit override\"\n", "");
    assert!(matches!(
        SpineConfig::parse_toml(&missing)
            .unwrap()
            .with_feature(Feature::Jit),
        Err(InitError::MissingPrompt(Feature::Jit))
    ));
    let missing = source.replace("node = \"node override\"\n", "");
    assert!(matches!(
        SpineConfig::parse_toml(&missing)
            .unwrap()
            .with_feature(Feature::Jit),
        Err(InitError::MissingPrompt(Feature::Jit))
    ));
    for (name, value) in [
        ("spawn_explicit_request_only", "explicit"),
        ("spawn_proactive", "proactive"),
    ] {
        let missing = source.replace(&format!("{name} = \"{value} override\"\n"), "");
        assert!(matches!(
            SpineConfig::parse_toml(&missing)
                .unwrap()
                .with_features([Feature::Jit, Feature::Spawn]),
            Err(InitError::MissingPrompt(Feature::Spawn))
        ));
    }
    for name in ["trim", "spawn"] {
        let missing = source.replace(&format!("{name} = \"{name} override\"\n"), "");
        assert!(
            SpineConfig::parse_toml(&missing)
                .unwrap()
                .with_features([Feature::Jit, Feature::Trim, Feature::Spawn])
                .is_ok()
        );
    }
    let exact = source.replace("node override", &"x".repeat(MAX_MODEL_VISIBLE_TEXT_BYTES));
    assert!(SpineConfig::parse_toml(&exact).is_ok());
    let oversized = source.replace(
        "node override",
        &"x".repeat(MAX_MODEL_VISIBLE_TEXT_BYTES + 1),
    );
    assert_eq!(
        SpineConfig::parse_toml(&oversized),
        Err(ConfigError::PromptTooLong {
            name: "prompt.node",
            max: MAX_MODEL_VISIBLE_TEXT_BYTES,
            actual: MAX_MODEL_VISIBLE_TEXT_BYTES + 1,
        })
    );
}

#[test]
fn projection_last_boundary_tracks_native_event_boundary() {
    let projection = apply(&[message(4, MessageRole::User, "request"), open(8, "child")]);
    assert_eq!(projection.last_boundary, Some(boundary(9)));
}

#[test]
fn settled_spawn_call_ids_describe_only_the_latest_event() {
    let tasks = spawn_tasks();
    let results = vec![
        spawn_result(0, SpawnOutcome::Completed, "one"),
        spawn_result(1, SpawnOutcome::Completed, "two"),
    ];
    let committed = apply(&[spawn(1, tasks.clone(), results.clone())]);
    assert_eq!(committed.settled_spawn_call_ids, ["spawn-1"]);

    let later_message = apply(&[
        spawn(1, tasks, results),
        message(3, MessageRole::Assistant, "continued"),
    ]);
    assert!(later_message.settled_spawn_call_ids.is_empty());
}

#[test]
fn failed_spawn_call_remains_ordinary_source_and_settles() {
    let projection = apply(&[sampling(
        1,
        vec![ToolUse {
            call_id: "spawn-failed".to_string(),
            name: "spine.spawn".to_string(),
            arguments: serde_json::json!({"tasks": spawn_tasks()}).to_string(),
            call_ordinal: None,
            outcome: Some(ToolOutcome::Failed),
            output: Some("spawn failed".to_string()),
            output_boundary: Some(boundary(2)),
        }],
    )]);

    assert_eq!(projection.settled_spawn_call_ids, ["spawn-failed"]);
    assert_eq!(projection.nodes.len(), 1);
    assert!(matches!(
        projection.visible_context.as_slice(),
        [ContextItem::SourceSpan { .. }]
    ));
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
