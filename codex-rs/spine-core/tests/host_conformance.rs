use pretty_assertions::assert_eq;
use spine_core::ContextEpoch;
use spine_core::ContextPlanRecipe;
use spine_core::ExecutionOrigin;
use spine_core::Feature;
use spine_core::Message;
use spine_core::MessageRole;
use spine_core::RawBoundary;
use spine_core::RecordDigest;
use spine_core::SamplingFinish;
use spine_core::SamplingRuntime;
use spine_core::SamplingTerminal;
use spine_core::SpineChar;
use spine_core::SpineConfig;
use spine_core::SpineOperationFact;
use spine_core::SpineProjection;
use spine_core::ThreadNamespace;
use spine_core::ToolOutcome;
use spine_core::ToolRequestChar;
use spine_core::ToolResponseChar;

#[derive(Clone)]
enum LogicalEvent {
    User(&'static str),
    Sampling(LogicalSampling),
}

#[derive(Clone)]
struct LogicalSampling {
    leading: Option<&'static str>,
    calls: Vec<LogicalCall>,
    execution: Option<ExecutionSpec>,
}

#[derive(Clone)]
struct LogicalCall {
    id: &'static str,
    name: &'static str,
    arguments: &'static str,
    outcome: ToolOutcome,
    output: Option<&'static str>,
}

#[derive(Clone)]
struct ExecutionSpec {
    call_id: &'static str,
    succeeded: bool,
    operation: Option<SpineOperationFact>,
}

#[derive(Clone)]
enum CodexResponseItem {
    Message(String),
    FunctionCall {
        call_id: String,
        name: String,
        arguments: String,
    },
    FunctionCallOutput {
        call_id: String,
        outcome: ToolOutcome,
        output: String,
    },
}

#[derive(Clone)]
enum CodexTurn {
    User(String),
    Sampling {
        output: Vec<CodexResponseItem>,
        execution: Option<ExecutionSpec>,
    },
}

#[derive(Clone)]
struct KimiToolCall {
    id: String,
    name: String,
    arguments: String,
}

#[derive(Clone)]
struct KimiToolOutput {
    call_id: String,
    outcome: ToolOutcome,
    output: String,
}

#[derive(Clone)]
enum KimiTurn {
    User(String),
    Sampling {
        assistant_text: Option<String>,
        calls: Vec<KimiToolCall>,
        outputs: Vec<KimiToolOutput>,
        execution: Option<ExecutionSpec>,
    },
}

enum HostAction {
    Source(SpineChar),
    Sampling {
        source: Vec<SpineChar>,
        execution: Option<ExecutionSpec>,
    },
}

#[derive(Debug, PartialEq, Eq)]
struct RunResult {
    projection: SpineProjection,
    preview: ContextPlanRecipe,
}

fn config() -> SpineConfig {
    let Ok(config) = SpineConfig::v1().with_features([Feature::Jit, Feature::Spawn, Feature::Trim])
    else {
        panic!("host conformance config must be valid");
    };
    config
}

fn namespace() -> ThreadNamespace {
    let Ok(namespace) = ThreadNamespace::parse("host-conformance") else {
        panic!("host conformance namespace must be valid");
    };
    namespace
}

fn encode_codex(events: &[LogicalEvent]) -> Vec<CodexTurn> {
    events
        .iter()
        .map(|event| match event {
            LogicalEvent::User(text) => CodexTurn::User((*text).to_string()),
            LogicalEvent::Sampling(sampling) => {
                let mut output = sampling
                    .leading
                    .map(|text| CodexResponseItem::Message(text.to_string()))
                    .into_iter()
                    .collect::<Vec<_>>();
                output.extend(
                    sampling
                        .calls
                        .iter()
                        .map(|call| CodexResponseItem::FunctionCall {
                            call_id: call.id.to_string(),
                            name: call.name.to_string(),
                            arguments: call.arguments.to_string(),
                        }),
                );
                output.extend(sampling.calls.iter().rev().filter_map(|call| {
                    call.output
                        .map(|body| CodexResponseItem::FunctionCallOutput {
                            call_id: call.id.to_string(),
                            outcome: call.outcome,
                            output: body.to_string(),
                        })
                }));
                CodexTurn::Sampling {
                    output,
                    execution: sampling.execution.clone(),
                }
            }
        })
        .collect()
}

fn encode_kimi(events: &[LogicalEvent]) -> Vec<KimiTurn> {
    events
        .iter()
        .map(|event| match event {
            LogicalEvent::User(text) => KimiTurn::User((*text).to_string()),
            LogicalEvent::Sampling(sampling) => KimiTurn::Sampling {
                assistant_text: sampling.leading.map(str::to_string),
                calls: sampling
                    .calls
                    .iter()
                    .map(|call| KimiToolCall {
                        id: call.id.to_string(),
                        name: call.name.to_string(),
                        arguments: call.arguments.to_string(),
                    })
                    .collect(),
                outputs: sampling
                    .calls
                    .iter()
                    .rev()
                    .filter_map(|call| {
                        call.output.map(|body| KimiToolOutput {
                            call_id: call.id.to_string(),
                            outcome: call.outcome,
                            output: body.to_string(),
                        })
                    })
                    .collect(),
                execution: sampling.execution.clone(),
            },
        })
        .collect()
}

fn adapt_codex(turns: &[CodexTurn]) -> Vec<HostAction> {
    let mut next_boundary = 1;
    turns
        .iter()
        .map(|turn| match turn {
            CodexTurn::User(text) => {
                HostAction::Source(message(&mut next_boundary, MessageRole::User, text.clone()))
            }
            CodexTurn::Sampling { output, execution } => {
                let source = output
                    .iter()
                    .map(|item| match item {
                        CodexResponseItem::Message(text) => {
                            message(&mut next_boundary, MessageRole::Assistant, text.clone())
                        }
                        CodexResponseItem::FunctionCall {
                            call_id,
                            name,
                            arguments,
                        } => request(
                            &mut next_boundary,
                            call_id.clone(),
                            name.clone(),
                            arguments.clone(),
                        ),
                        CodexResponseItem::FunctionCallOutput {
                            call_id,
                            outcome,
                            output,
                        } => response(
                            &mut next_boundary,
                            call_id.clone(),
                            *outcome,
                            output.clone(),
                        ),
                    })
                    .collect();
                HostAction::Sampling {
                    source,
                    execution: execution.clone(),
                }
            }
        })
        .collect()
}

fn adapt_kimi(turns: &[KimiTurn]) -> Vec<HostAction> {
    let mut next_boundary = 1;
    turns
        .iter()
        .map(|turn| match turn {
            KimiTurn::User(text) => {
                HostAction::Source(message(&mut next_boundary, MessageRole::User, text.clone()))
            }
            KimiTurn::Sampling {
                assistant_text,
                calls,
                outputs,
                execution,
            } => {
                let mut source = assistant_text
                    .iter()
                    .map(|text| message(&mut next_boundary, MessageRole::Assistant, text.clone()))
                    .collect::<Vec<_>>();
                source.extend(calls.iter().map(|call| {
                    request(
                        &mut next_boundary,
                        call.id.clone(),
                        call.name.clone(),
                        call.arguments.clone(),
                    )
                }));
                source.extend(outputs.iter().map(|output| {
                    response(
                        &mut next_boundary,
                        output.call_id.clone(),
                        output.outcome,
                        output.output.clone(),
                    )
                }));
                HostAction::Sampling {
                    source,
                    execution: execution.clone(),
                }
            }
        })
        .collect()
}

fn message(boundary: &mut u64, role: MessageRole, content: String) -> SpineChar {
    let character = SpineChar::Message(Message {
        boundary: RawBoundary(*boundary),
        role,
        content,
    });
    *boundary = boundary.saturating_add(1);
    character
}

fn request(boundary: &mut u64, call_id: String, name: String, arguments: String) -> SpineChar {
    let character = SpineChar::ToolRequest(ToolRequestChar {
        boundary: RawBoundary(*boundary),
        call_id,
        name,
        arguments,
    });
    *boundary = boundary.saturating_add(1);
    character
}

fn response(
    boundary: &mut u64,
    call_id: String,
    outcome: ToolOutcome,
    output: String,
) -> SpineChar {
    let character = SpineChar::ToolResponse(ToolResponseChar {
        boundary: RawBoundary(*boundary),
        call_id,
        outcome,
        output,
    });
    *boundary = boundary.saturating_add(1);
    character
}

fn run(actions: Vec<HostAction>) -> Result<RunResult, String> {
    let mut runtime = SamplingRuntime::new(namespace(), ContextEpoch::ZERO, config())
        .map_err(|error| error.to_string())?;
    for action in actions {
        match action {
            HostAction::Source(source) => {
                runtime
                    .observe_source([source])
                    .map_err(|error| error.to_string())?;
            }
            HostAction::Sampling { source, execution } => {
                let handle = runtime
                    .begin_sampling()
                    .map_err(|error| error.to_string())?;
                runtime
                    .sampling_started_record(&handle, RecordDigest::digest(b"prompt"))
                    .map_err(|error| error.to_string())?;
                if let Some(execution) = &execution {
                    runtime
                        .register_execution(execution.call_id)
                        .map_err(|error| error.to_string())?;
                    if let Some(operation) = &execution.operation {
                        runtime
                            .stage_execution(
                                execution.call_id,
                                ExecutionOrigin::Direct {
                                    call_id: execution.call_id.to_string(),
                                },
                                operation.clone(),
                            )
                            .map_err(|error| error.to_string())?;
                    }
                }
                runtime
                    .observe_source(source)
                    .map_err(|error| error.to_string())?;
                if let Some(execution) = execution {
                    runtime
                        .finish_execution(execution.call_id, execution.succeeded)
                        .map_err(|error| error.to_string())?;
                }
                let SamplingFinish::Prepared(prepared) = runtime
                    .finish_sampling(handle, SamplingTerminal::Completed)
                    .map_err(|error| error.to_string())?
                else {
                    return Err("completed sampling became orphaned".to_string());
                };
                runtime
                    .install_prepared(prepared)
                    .map_err(|error| error.to_string())?;
            }
        }
    }
    Ok(RunResult {
        projection: runtime.projection().clone(),
        preview: runtime
            .preview_context_plan()
            .map_err(|error| error.to_string())?,
    })
}

fn run_both(events: &[LogicalEvent]) -> (Result<RunResult, String>, Result<RunResult, String>) {
    (
        run(adapt_codex(&encode_codex(events))),
        run(adapt_kimi(&encode_kimi(events))),
    )
}

fn call(
    id: &'static str,
    name: &'static str,
    outcome: ToolOutcome,
    output: Option<&'static str>,
) -> LogicalCall {
    LogicalCall {
        id,
        name,
        arguments: "{}",
        outcome,
        output,
    }
}

fn transcript() -> Vec<LogicalEvent> {
    vec![
        LogicalEvent::User("root request"),
        LogicalEvent::Sampling(LogicalSampling {
            leading: Some("reasoning before parallel calls"),
            calls: vec![
                call("ordinary-a", "shell", ToolOutcome::Succeeded, Some("a")),
                call("open", "spine.open", ToolOutcome::Succeeded, Some("ok")),
                call("ordinary-b", "shell", ToolOutcome::Failed, Some("b")),
            ],
            execution: Some(ExecutionSpec {
                call_id: "open",
                succeeded: true,
                operation: Some(SpineOperationFact::Open {
                    summary: "child".to_string(),
                }),
            }),
        }),
        LogicalEvent::User("child request"),
        LogicalEvent::Sampling(LogicalSampling {
            leading: None,
            calls: vec![call(
                "next",
                "spine.next",
                ToolOutcome::Succeeded,
                Some("ok"),
            )],
            execution: Some(ExecutionSpec {
                call_id: "next",
                succeeded: true,
                operation: Some(SpineOperationFact::Next {
                    closed_memory: "first child done".to_string(),
                    next_summary: "sibling".to_string(),
                }),
            }),
        }),
        LogicalEvent::Sampling(LogicalSampling {
            leading: None,
            calls: vec![call(
                "close",
                "spine.close",
                ToolOutcome::Succeeded,
                Some("ok"),
            )],
            execution: Some(ExecutionSpec {
                call_id: "close",
                succeeded: true,
                operation: Some(SpineOperationFact::Close {
                    memory: "second child done".to_string(),
                }),
            }),
        }),
    ]
}

#[test]
fn codex_and_kimi_execute_the_same_sampling_transactions() {
    let (codex, kimi) = run_both(&transcript());
    assert_eq!(codex.unwrap(), kimi.unwrap());
}

#[test]
fn codex_and_kimi_match_failed_and_incomplete_tool_samplings() {
    let failed = [LogicalEvent::Sampling(LogicalSampling {
        leading: Some("failed control"),
        calls: vec![call(
            "failed-open",
            "spine.open",
            ToolOutcome::Failed,
            Some("failed"),
        )],
        execution: Some(ExecutionSpec {
            call_id: "failed-open",
            succeeded: false,
            operation: None,
        }),
    })];
    let (codex, kimi) = run_both(&failed);
    assert_eq!(codex.unwrap(), kimi.unwrap());

    let incomplete = [LogicalEvent::Sampling(LogicalSampling {
        leading: None,
        calls: vec![call("incomplete", "shell", ToolOutcome::Succeeded, None)],
        execution: None,
    })];
    let (codex, kimi) = run_both(&incomplete);
    let codex = codex.unwrap_err();
    let kimi = kimi.unwrap_err();
    assert_eq!(codex, kimi);
    assert!(codex.contains("incomplete tool group"));
}

#[test]
fn codex_and_kimi_resume_and_rollback_to_the_same_prefixes() {
    let events = transcript();
    for prefix_len in 0..=events.len() {
        let (codex, kimi) = run_both(&events[..prefix_len]);
        assert_eq!(codex.unwrap(), kimi.unwrap(), "prefix {prefix_len}");
    }
}
