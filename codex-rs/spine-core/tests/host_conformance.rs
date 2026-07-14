use codex_spine_core::ContextItem;
use codex_spine_core::Message;
use codex_spine_core::MessageRole;
use codex_spine_core::RawBoundary;
use codex_spine_core::RolloutEvent;
use codex_spine_core::SpineProjection;
use codex_spine_core::SpineReducer;
use codex_spine_core::ToolCallGroup;
use codex_spine_core::ToolOutcome;
use codex_spine_core::ToolUse;
use pretty_assertions::assert_eq;

#[derive(Clone)]
enum LogicalEvent {
    User(&'static str),
    Assistant(&'static str),
    ToolTurn {
        leading: Option<&'static str>,
        calls: Vec<LogicalCall>,
    },
    Compact(&'static str),
}

#[derive(Clone)]
struct LogicalCall {
    id: &'static str,
    name: &'static str,
    arguments: &'static str,
    success: Option<bool>,
    output: Option<&'static str>,
}

impl LogicalCall {
    fn success(id: &'static str, name: &'static str, arguments: &'static str) -> Self {
        Self {
            id,
            name,
            arguments,
            success: Some(true),
            output: Some("ok"),
        }
    }

    fn failed(id: &'static str, name: &'static str, arguments: &'static str) -> Self {
        Self {
            id,
            name,
            arguments,
            success: Some(false),
            output: Some("failed"),
        }
    }
}

#[derive(Clone)]
enum CodexResponseItem {
    Message {
        role: MessageRole,
        text: String,
    },
    FunctionCall {
        call_id: String,
        name: String,
        arguments: String,
    },
    FunctionCallOutput {
        call_id: String,
        success: Option<bool>,
        output: Option<String>,
    },
}

#[derive(Clone)]
enum CodexRolloutItem {
    Response {
        ordinal: u64,
        output: Vec<CodexResponseItem>,
    },
    Compacted {
        ordinal: u64,
        replacement_history: Vec<CodexResponseItem>,
    },
}

#[derive(Clone)]
enum KimiContextMessage {
    User {
        text: String,
    },
    Assistant {
        text: Option<String>,
        calls: Vec<KimiToolCall>,
    },
    Tool {
        call_id: String,
        success: Option<bool>,
        output: Option<String>,
    },
    CompactSummary {
        text: String,
    },
}

#[derive(Clone)]
struct KimiToolCall {
    id: String,
    name: String,
    arguments: String,
}

#[derive(Clone)]
struct KimiContextRecord {
    ordinal: u64,
    messages: Vec<KimiContextMessage>,
    compact: bool,
}

fn encode_codex(events: &[LogicalEvent]) -> Vec<CodexRolloutItem> {
    events
        .iter()
        .enumerate()
        .map(|(index, event)| {
            let ordinal = index as u64 * 10 + 1;
            match event {
                LogicalEvent::User(text) => CodexRolloutItem::Response {
                    ordinal,
                    output: vec![CodexResponseItem::Message {
                        role: MessageRole::User,
                        text: (*text).to_string(),
                    }],
                },
                LogicalEvent::Assistant(text) => CodexRolloutItem::Response {
                    ordinal,
                    output: vec![CodexResponseItem::Message {
                        role: MessageRole::Assistant,
                        text: (*text).to_string(),
                    }],
                },
                LogicalEvent::ToolTurn { leading, calls } => {
                    let mut output = Vec::new();
                    if let Some(text) = leading {
                        output.push(CodexResponseItem::Message {
                            role: MessageRole::Assistant,
                            text: (*text).to_string(),
                        });
                    }
                    output.extend(calls.iter().map(|call| CodexResponseItem::FunctionCall {
                        call_id: call.id.to_string(),
                        name: call.name.to_string(),
                        arguments: call.arguments.to_string(),
                    }));
                    output.extend(
                        calls
                            .iter()
                            .map(|call| CodexResponseItem::FunctionCallOutput {
                                call_id: call.id.to_string(),
                                success: call.success,
                                output: call.output.map(str::to_string),
                            }),
                    );
                    CodexRolloutItem::Response { ordinal, output }
                }
                LogicalEvent::Compact(text) => CodexRolloutItem::Compacted {
                    ordinal,
                    replacement_history: vec![CodexResponseItem::Message {
                        role: MessageRole::Assistant,
                        text: (*text).to_string(),
                    }],
                },
            }
        })
        .collect()
}

fn encode_kimi(events: &[LogicalEvent]) -> Vec<KimiContextRecord> {
    events
        .iter()
        .enumerate()
        .map(|(index, event)| {
            let ordinal = index as u64 * 10 + 1;
            match event {
                LogicalEvent::User(text) => KimiContextRecord {
                    ordinal,
                    messages: vec![KimiContextMessage::User {
                        text: (*text).to_string(),
                    }],
                    compact: false,
                },
                LogicalEvent::Assistant(text) => KimiContextRecord {
                    ordinal,
                    messages: vec![KimiContextMessage::Assistant {
                        text: Some((*text).to_string()),
                        calls: Vec::new(),
                    }],
                    compact: false,
                },
                LogicalEvent::ToolTurn { leading, calls } => {
                    let assistant = KimiContextMessage::Assistant {
                        text: leading.map(str::to_string),
                        calls: calls
                            .iter()
                            .map(|call| KimiToolCall {
                                id: call.id.to_string(),
                                name: call.name.to_string(),
                                arguments: call.arguments.to_string(),
                            })
                            .collect(),
                    };
                    let mut messages = vec![assistant];
                    messages.extend(calls.iter().map(|call| KimiContextMessage::Tool {
                        call_id: call.id.to_string(),
                        success: call.success,
                        output: call.output.map(str::to_string),
                    }));
                    KimiContextRecord {
                        ordinal,
                        messages,
                        compact: false,
                    }
                }
                LogicalEvent::Compact(text) => KimiContextRecord {
                    ordinal,
                    messages: vec![KimiContextMessage::CompactSummary {
                        text: (*text).to_string(),
                    }],
                    compact: true,
                },
            }
        })
        .collect()
}

fn adapt_codex(items: &[CodexRolloutItem]) -> Vec<RolloutEvent> {
    items
        .iter()
        .map(|item| match item {
            CodexRolloutItem::Response { ordinal, output } => {
                let calls: Vec<_> = output
                    .iter()
                    .filter_map(|item| match item {
                        CodexResponseItem::FunctionCall {
                            call_id,
                            name,
                            arguments,
                        } => {
                            let result = output.iter().find_map(|candidate| match candidate {
                                CodexResponseItem::FunctionCallOutput {
                                    call_id: output_id,
                                    success,
                                    output,
                                } if output_id == call_id => Some((*success, output.clone())),
                                _ => None,
                            });
                            Some(ToolUse {
                                call_id: call_id.clone(),
                                name: name.clone(),
                                arguments: arguments.clone(),
                                outcome: result.as_ref().map(|(success, _)| map_outcome(*success)),
                                output: result.and_then(|(_, output)| output),
                                output_boundary: None,
                            })
                        }
                        _ => None,
                    })
                    .collect();
                if calls.is_empty() {
                    let CodexResponseItem::Message { role, text } = &output[0] else {
                        panic!("response without calls must contain one message");
                    };
                    RolloutEvent::Message(Message {
                        boundary: RawBoundary(*ordinal),
                        role: *role,
                        content: text.clone(),
                    })
                } else {
                    let leading_assistant_messages = output
                        .iter()
                        .filter_map(|item| match item {
                            CodexResponseItem::Message { role, text } => Some(Message {
                                boundary: RawBoundary(*ordinal),
                                role: *role,
                                content: text.clone(),
                            }),
                            _ => None,
                        })
                        .collect();
                    RolloutEvent::ToolCall(ToolCallGroup {
                        start: RawBoundary(*ordinal),
                        end: RawBoundary(*ordinal + 1),
                        leading_assistant_messages,
                        calls,
                    })
                }
            }
            CodexRolloutItem::Compacted {
                ordinal,
                replacement_history,
            } => RolloutEvent::Compact {
                boundary: RawBoundary(*ordinal),
                replacement_history: replacement_history
                    .iter()
                    .map(|item| message_context(*ordinal, item))
                    .collect(),
            },
        })
        .collect()
}

fn adapt_kimi(records: &[KimiContextRecord]) -> Vec<RolloutEvent> {
    records
        .iter()
        .map(|record| {
            if record.compact {
                let [KimiContextMessage::CompactSummary { text }] = record.messages.as_slice()
                else {
                    panic!("compact record must contain one summary");
                };
                return RolloutEvent::Compact {
                    boundary: RawBoundary(record.ordinal),
                    replacement_history: vec![ContextItem::Message {
                        message: Message {
                            boundary: RawBoundary(record.ordinal),
                            role: MessageRole::Assistant,
                            content: text.clone(),
                        },
                        user_anchor: None,
                    }],
                };
            }

            match &record.messages[0] {
                KimiContextMessage::User { text } => RolloutEvent::Message(Message {
                    boundary: RawBoundary(record.ordinal),
                    role: MessageRole::User,
                    content: text.clone(),
                }),
                KimiContextMessage::Assistant { text, calls } if calls.is_empty() => {
                    RolloutEvent::Message(Message {
                        boundary: RawBoundary(record.ordinal),
                        role: MessageRole::Assistant,
                        content: text.clone().unwrap_or_default(),
                    })
                }
                KimiContextMessage::Assistant { text, calls } => {
                    let tool_uses = calls
                        .iter()
                        .map(|call| {
                            let result =
                                record
                                    .messages
                                    .iter()
                                    .find_map(|candidate| match candidate {
                                        KimiContextMessage::Tool {
                                            call_id,
                                            success,
                                            output,
                                        } if call_id == &call.id => {
                                            Some((*success, output.clone()))
                                        }
                                        _ => None,
                                    });
                            ToolUse {
                                call_id: call.id.clone(),
                                name: call.name.clone(),
                                arguments: call.arguments.clone(),
                                outcome: result.as_ref().map(|(success, _)| map_outcome(*success)),
                                output: result.and_then(|(_, output)| output),
                                output_boundary: None,
                            }
                        })
                        .collect();
                    let leading_assistant_messages = text
                        .iter()
                        .map(|text| Message {
                            boundary: RawBoundary(record.ordinal),
                            role: MessageRole::Assistant,
                            content: text.clone(),
                        })
                        .collect();
                    RolloutEvent::ToolCall(ToolCallGroup {
                        start: RawBoundary(record.ordinal),
                        end: RawBoundary(record.ordinal + 1),
                        leading_assistant_messages,
                        calls: tool_uses,
                    })
                }
                KimiContextMessage::Tool { .. } | KimiContextMessage::CompactSummary { .. } => {
                    panic!("record cannot begin with standalone tool output or summary")
                }
            }
        })
        .collect()
}

fn map_outcome(success: Option<bool>) -> ToolOutcome {
    match success {
        Some(true) => ToolOutcome::Succeeded,
        Some(false) => ToolOutcome::Failed,
        None => ToolOutcome::Unknown,
    }
}

fn message_context(ordinal: u64, item: &CodexResponseItem) -> ContextItem {
    let CodexResponseItem::Message { role, text } = item else {
        panic!("replacement fixture contains only messages");
    };
    ContextItem::Message {
        message: Message {
            boundary: RawBoundary(ordinal),
            role: *role,
            content: text.clone(),
        },
        user_anchor: None,
    }
}

fn projections(events: &[LogicalEvent]) -> (SpineProjection, SpineProjection) {
    let codex = SpineReducer::derive(&adapt_codex(&encode_codex(events)));
    let kimi = SpineReducer::derive(&adapt_kimi(&encode_kimi(events)));
    (codex, kimi)
}

fn assert_conforms(name: &str, events: &[LogicalEvent], expected_cursor: &str) {
    let (codex, kimi) = projections(events);
    assert_eq!(codex, kimi, "host projection mismatch for {name}");
    assert_eq!(codex.cursor.to_string(), expected_cursor, "case {name}");
}

#[test]
fn codex_and_kimi_fixture_adapters_conform() {
    let open = LogicalCall::success("open", "spine.open", r#"{"summary":"task"}"#);
    let nested = LogicalCall::success("nested", "spine.open", r#"{"summary":"nested"}"#);
    let close = LogicalCall::success("close", "spine.close", r#"{"memory":"done"}"#);
    let next = LogicalCall::success(
        "next",
        "spine.next",
        r#"{"summary":"sibling","memory":"done"}"#,
    );
    let ordinary = LogicalCall::success("shell", "shell", r#"{"cmd":"pwd"}"#);
    let failed = LogicalCall::failed("failed", "spine.open", r#"{"summary":"ignored"}"#);

    let cases = vec![
        (
            "open",
            vec![LogicalEvent::ToolTurn {
                leading: Some("opening"),
                calls: vec![open.clone()],
            }],
            "1.1",
        ),
        (
            "close",
            vec![
                LogicalEvent::ToolTurn {
                    leading: None,
                    calls: vec![open.clone()],
                },
                LogicalEvent::User("work"),
                LogicalEvent::ToolTurn {
                    leading: None,
                    calls: vec![close.clone()],
                },
            ],
            "1",
        ),
        (
            "next",
            vec![
                LogicalEvent::ToolTurn {
                    leading: None,
                    calls: vec![open.clone()],
                },
                LogicalEvent::ToolTurn {
                    leading: None,
                    calls: vec![next],
                },
            ],
            "1.2",
        ),
        (
            "nested close",
            vec![
                LogicalEvent::ToolTurn {
                    leading: None,
                    calls: vec![open.clone()],
                },
                LogicalEvent::ToolTurn {
                    leading: None,
                    calls: vec![nested],
                },
                LogicalEvent::ToolTurn {
                    leading: None,
                    calls: vec![close.clone()],
                },
            ],
            "1.1",
        ),
        (
            "compact",
            vec![
                LogicalEvent::User("old request"),
                LogicalEvent::Compact("native compact summary"),
            ],
            "2",
        ),
        (
            "failed control",
            vec![LogicalEvent::ToolTurn {
                leading: None,
                calls: vec![failed],
            }],
            "1",
        ),
        (
            "ordinary coexists",
            vec![LogicalEvent::ToolTurn {
                leading: Some("inspect then open"),
                calls: vec![ordinary.clone(), open.clone()],
            }],
            "1.1",
        ),
        (
            "ordinary only",
            vec![
                LogicalEvent::Assistant("inspect"),
                LogicalEvent::ToolTurn {
                    leading: None,
                    calls: vec![ordinary],
                },
            ],
            "1",
        ),
    ];

    for (name, events, cursor) in cases {
        assert_conforms(name, &events, cursor);
    }
}

#[test]
fn codex_and_kimi_resume_from_full_native_transcript() {
    let events = vec![
        LogicalEvent::User("request"),
        LogicalEvent::ToolTurn {
            leading: None,
            calls: vec![LogicalCall::success(
                "open",
                "spine.open",
                r#"{"summary":"task"}"#,
            )],
        },
        LogicalEvent::User("detail"),
        LogicalEvent::ToolTurn {
            leading: None,
            calls: vec![LogicalCall::success(
                "close",
                "spine.close",
                r#"{"memory":"done"}"#,
            )],
        },
    ];
    let (live_codex, live_kimi) = projections(&events);
    let resumed_codex = SpineReducer::derive(&adapt_codex(&encode_codex(&events)));
    let resumed_kimi = SpineReducer::derive(&adapt_kimi(&encode_kimi(&events)));
    assert_eq!(live_codex, resumed_codex);
    assert_eq!(live_kimi, resumed_kimi);
    assert_eq!(resumed_codex, resumed_kimi);
}

#[test]
fn codex_and_kimi_rollback_use_the_same_native_prefix() {
    let events = vec![
        LogicalEvent::User("request"),
        LogicalEvent::ToolTurn {
            leading: None,
            calls: vec![LogicalCall::success(
                "open",
                "spine.open",
                r#"{"summary":"task"}"#,
            )],
        },
        LogicalEvent::User("rolled back detail"),
    ];
    for prefix_len in 0..=events.len() {
        let prefix = &events[..prefix_len];
        let (codex, kimi) = projections(prefix);
        assert_eq!(codex, kimi, "rollback prefix {prefix_len}");
    }
}

#[test]
fn codex_and_kimi_incomplete_outputs_are_non_transitions() {
    let events = vec![LogicalEvent::ToolTurn {
        leading: None,
        calls: vec![LogicalCall {
            id: "open",
            name: "spine.open",
            arguments: r#"{"summary":"task"}"#,
            success: None,
            output: None,
        }],
    }];
    assert_conforms("incomplete output", &events, "1");
}
