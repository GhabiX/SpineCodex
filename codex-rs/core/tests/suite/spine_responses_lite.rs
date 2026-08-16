use anyhow::Context;
use anyhow::Result;
use codex_core::StartThreadOptions;
use codex_features::Feature;
use codex_protocol::dynamic_tools::DynamicToolFunctionSpec;
use codex_protocol::dynamic_tools::DynamicToolNamespaceSpec;
use codex_protocol::dynamic_tools::DynamicToolNamespaceTool;
use codex_protocol::dynamic_tools::DynamicToolSpec;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::RolloutLine;
use core_test_support::hooks::trust_discovered_hooks;
use core_test_support::responses;
use core_test_support::skip_if_no_network;
use core_test_support::streaming_sse::StreamingSseChunk;
use core_test_support::streaming_sse::start_streaming_sse_server;
use core_test_support::test_codex::spine_test_codex;
use core_test_support::test_codex::test_codex;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;

fn write_first_spine_open_blocking_post_hook(home: &std::path::Path) -> Result<()> {
    let script_path = home.join("post_tool_use_spine_open.py");
    let log_path = home.join("post_tool_use_spine_open.jsonl");
    let script = format!(
        r#"import json
from pathlib import Path
import sys

log_path = Path(r"{log_path}")
payload = json.load(sys.stdin)
invocation_index = 0
if log_path.exists():
    invocation_index = len(log_path.read_text(encoding="utf-8").splitlines())
with log_path.open("a", encoding="utf-8") as handle:
    handle.write(json.dumps(payload) + "\n")
if invocation_index == 0:
    print(json.dumps({{"decision": "block", "reason": "first open blocked by test hook"}}))
"#,
        log_path = log_path.display(),
    );
    let hooks = serde_json::json!({
        "hooks": {
            "PostToolUse": [{
                "matcher": "^spineopen$",
                "hooks": [{
                    "type": "command",
                    "command": format!("python3 {}", script_path.display()),
                }]
            }]
        }
    });
    std::fs::write(&script_path, script).context("write Spine PostToolUse hook script")?;
    std::fs::write(home.join("hooks.json"), hooks.to_string())
        .context("write Spine PostToolUse hook config")?;
    Ok(())
}

fn has_namespaced_tool(tools: &[Value], namespace: &str, tool_name: &str) -> bool {
    tools.iter().any(|tool| {
        tool.get("type").and_then(Value::as_str) == Some("namespace")
            && tool.get("name").and_then(Value::as_str) == Some(namespace)
            && tool["tools"].as_array().is_some_and(|tools| {
                tools
                    .iter()
                    .any(|tool| tool.get("name").and_then(Value::as_str) == Some(tool_name))
            })
    })
}

fn additional_tools(body: &Value) -> Result<Vec<Value>> {
    let input = body["input"]
        .as_array()
        .context("Responses request input should be an array")?;
    let tools = input
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("additional_tools"))
        .flat_map(|item| item["tools"].as_array().into_iter().flatten().cloned())
        .collect::<Vec<_>>();
    if tools.is_empty() {
        anyhow::bail!("Responses request should contain additional_tools");
    }
    Ok(tools)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_spine_controls_admit_only_the_first_valid_native_ordinal() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let response_mock = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                responses::ev_response_created("resp-direct-ordinal"),
                responses::ev_function_call_with_namespace(
                    "direct-open-first",
                    "spine",
                    "open",
                    r#"{"summary":"first child"}"#,
                ),
                responses::ev_function_call_with_namespace(
                    "direct-open-second",
                    "spine",
                    "open",
                    r#"{"summary":"second child"}"#,
                ),
                responses::ev_completed("resp-direct-ordinal"),
            ]),
            responses::sse(vec![
                responses::ev_assistant_message("msg-direct-ordinal", "done"),
                responses::ev_completed("resp-direct-ordinal-followup"),
            ]),
        ],
    )
    .await;
    let test = spine_test_codex()
        .with_model_info_override("gpt-5.4", |model_info| {
            model_info.use_responses_lite = true;
        })
        .build(&server)
        .await?;

    test.submit_turn("run two direct Spine controls").await?;

    let requests = response_mock.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1]
            .function_call_output_text("direct-open-first")
            .as_deref(),
        Some("Spine open accepted.")
    );
    let second = requests[1]
        .function_call_output_text("direct-open-second")
        .expect("second direct control output");
    assert!(second.contains("already has a validated Spine control"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_spine_controls_skip_runtime_invalid_earlier_call() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let response_mock = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                responses::ev_response_created("resp-direct-invalid-first"),
                responses::ev_function_call_with_namespace(
                    "direct-close-root",
                    "spine",
                    "close",
                    r#"{"memory":"root cannot close"}"#,
                ),
                responses::ev_function_call_with_namespace(
                    "direct-open-after-invalid",
                    "spine",
                    "open",
                    r#"{"summary":"valid child"}"#,
                ),
                responses::ev_completed("resp-direct-invalid-first"),
            ]),
            responses::sse(vec![
                responses::ev_assistant_message("msg-direct-invalid-first", "done"),
                responses::ev_completed("resp-direct-invalid-first-followup"),
            ]),
        ],
    )
    .await;
    let test = spine_test_codex()
        .with_model_info_override("gpt-5.4", |model_info| {
            model_info.use_responses_lite = true;
        })
        .build(&server)
        .await?;

    test.submit_turn("skip invalid direct Spine control")
        .await?;

    let requests = response_mock.requests();
    assert_eq!(requests.len(), 2);
    let close_root = requests[1]
        .function_call_output_text("direct-close-root")
        .context("invalid root close output")?;
    assert!(close_root.contains("no open Spine node is available to close"));
    assert_eq!(
        requests[1]
            .function_call_output_text("direct-open-after-invalid")
            .as_deref(),
        Some("Spine open accepted.")
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_spine_control_post_hook_failure_releases_next_ordinal() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let response_mock = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                responses::ev_response_created("resp-direct-post-hook"),
                responses::ev_function_call_with_namespace(
                    "direct-open-blocked",
                    "spine",
                    "open",
                    r#"{"summary":"blocked child"}"#,
                ),
                responses::ev_function_call_with_namespace(
                    "direct-open-after-hook",
                    "spine",
                    "open",
                    r#"{"summary":"hook survivor"}"#,
                ),
                responses::ev_completed("resp-direct-post-hook"),
            ]),
            responses::sse(vec![
                responses::ev_assistant_message("msg-direct-post-hook", "done"),
                responses::ev_completed("resp-direct-post-hook-followup"),
            ]),
        ],
    )
    .await;
    let test = spine_test_codex()
        .with_model_info_override("gpt-5.4", |model_info| {
            model_info.use_responses_lite = true;
        })
        .with_pre_build_hook(|home| {
            write_first_spine_open_blocking_post_hook(home)
                .expect("write blocking Spine PostToolUse hook fixture");
        })
        .with_config(trust_discovered_hooks)
        .build(&server)
        .await?;

    test.submit_turn("run direct Spine controls through a blocking post hook")
        .await?;

    let requests = response_mock.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1]
            .function_call_output_text("direct-open-blocked")
            .as_deref(),
        Some("first open blocked by test hook")
    );
    assert_eq!(
        requests[1]
            .function_call_output_text("direct-open-after-hook")
            .as_deref(),
        Some("Spine open accepted.")
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sampling_source_is_projected_once_in_order() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let response_mock = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                responses::ev_response_created("sampling-source-open"),
                responses::ev_function_call_with_namespace(
                    "sampling-source-open-call",
                    "spine",
                    "open",
                    r#"{"summary":"sampling source child"}"#,
                ),
                responses::ev_completed("sampling-source-open"),
            ]),
            responses::sse(vec![
                responses::ev_response_created("sampling-source-tools"),
                responses::ev_reasoning_item(
                    "sampling-source-reasoning",
                    &["sampling source reasoning"],
                    &[],
                ),
                responses::ev_function_call(
                    "sampling-source-first-call",
                    "shell_command",
                    &serde_json::json!({"command": "echo sampling-source-first"}).to_string(),
                ),
                responses::ev_function_call(
                    "sampling-source-second-call",
                    "shell_command",
                    &serde_json::json!({"command": "echo sampling-source-second"}).to_string(),
                ),
                responses::ev_completed("sampling-source-tools"),
            ]),
            responses::sse(vec![
                responses::ev_response_created("sampling-source-close"),
                responses::ev_function_call_with_namespace(
                    "sampling-source-close-call",
                    "spine",
                    "close",
                    r#"{"memory":"sampling source complete"}"#,
                ),
                responses::ev_completed("sampling-source-close"),
            ]),
            responses::sse(vec![
                responses::ev_response_created("sampling-source-done"),
                responses::ev_completed("sampling-source-done"),
            ]),
        ],
    )
    .await;
    let test = spine_test_codex()
        .with_model_info_override("gpt-5.4", |model_info| {
            model_info.use_responses_lite = true;
        })
        .build(&server)
        .await?;

    test.submit_turn("project one complete sampling source")
        .await?;

    let requests = response_mock.requests();
    assert_eq!(requests.len(), 4);
    let close_input = requests[2].input();
    let reasoning_index = close_input
        .iter()
        .position(|item| item.get("type").and_then(Value::as_str) == Some("reasoning"))
        .context("missing leading reasoning item")?;
    let first_call_index = close_input
        .iter()
        .position(|item| {
            item.get("call_id").and_then(Value::as_str) == Some("sampling-source-first-call")
        })
        .context("missing first tool request")?;
    let second_call_index = close_input
        .iter()
        .position(|item| {
            item.get("call_id").and_then(Value::as_str) == Some("sampling-source-second-call")
        })
        .context("missing second tool request")?;
    let first_output_index = close_input
        .iter()
        .position(|item| {
            item.get("type").and_then(Value::as_str) == Some("function_call_output")
                && item.get("call_id").and_then(Value::as_str) == Some("sampling-source-first-call")
        })
        .context("missing first tool output")?;
    let second_output_index = close_input
        .iter()
        .position(|item| {
            item.get("type").and_then(Value::as_str) == Some("function_call_output")
                && item.get("call_id").and_then(Value::as_str)
                    == Some("sampling-source-second-call")
        })
        .context("missing second tool output")?;
    assert_eq!(
        [
            reasoning_index,
            first_call_index,
            second_call_index,
            first_output_index,
            second_output_index,
        ],
        [
            reasoning_index,
            reasoning_index + 1,
            reasoning_index + 2,
            reasoning_index + 3,
            reasoning_index + 4,
        ]
    );

    let rendered = serde_json::to_string(&requests[3].input())?;
    assert!(rendered.contains("sampling source complete"));
    assert!(rendered.contains("sampling-source-close-call"));
    assert!(!rendered.contains("sampling-source-reasoning"));
    assert!(!rendered.contains("sampling-source-first-call"));
    assert!(!rendered.contains("sampling-source-second-call"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sampling_retry_preserves_input_and_commits_only_the_successful_attempt() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let failed_stream = responses::sse(vec![responses::ev_response_created(
        "sampling-retry-failed",
    )]);
    let successful_stream = responses::sse(vec![
        responses::ev_response_created("sampling-retry-success"),
        responses::ev_assistant_message("sampling-retry-message", "retry complete"),
        responses::ev_completed("sampling-retry-success"),
    ]);
    let (server, _) = start_streaming_sse_server(vec![
        vec![StreamingSseChunk {
            gate: None,
            body: failed_stream,
        }],
        vec![StreamingSseChunk {
            gate: None,
            body: successful_stream,
        }],
    ])
    .await;
    let mut builder = spine_test_codex().with_config(|config| {
        config.model_provider.request_max_retries = Some(0);
        config.model_provider.stream_max_retries = Some(1);
        config.model_provider.supports_websockets = false;
    });
    let test = builder.build_with_streaming_server(&server).await?;

    test.submit_turn("retry this exact Spine sampling input")
        .await?;

    let requests = server.requests().await;
    assert_eq!(requests.len(), 2, "expected one normal stream retry");
    let request_bodies = requests
        .iter()
        .map(|body| serde_json::from_slice::<Value>(body))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(
        request_bodies[0]["input"], request_bodies[1]["input"],
        "the retry must preserve the first attempt input exactly"
    );
    assert!(
        !request_bodies[1]
            .to_string()
            .contains("sampling-retry-failed")
    );

    test.codex.flush_rollout().await?;
    let rollout_path = test.codex.rollout_path().context("rollout path")?;
    let rollout = std::fs::read_to_string(&rollout_path)
        .with_context(|| format!("read rollout {}", rollout_path.display()))?;
    let items = rollout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(serde_json::from_str::<RolloutLine>)
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .map(|line| line.item)
        .collect::<Vec<_>>();
    assert_eq!(
        items
            .iter()
            .filter(|item| matches!(item, RolloutItem::SpineSamplingStarted(_)))
            .count(),
        2,
        "each admitted sampling attempt must be durable"
    );
    assert_eq!(
        items
            .iter()
            .filter(|item| matches!(item, RolloutItem::SpineTransition(_)))
            .count(),
        1,
        "the empty failed attempt must not publish a canonical transition"
    );

    server.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spine_tools_remain_native_only_when_code_mode_is_enabled() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let response_mock = responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_response_created("resp-tools"),
            responses::ev_completed("resp-tools"),
        ]),
    )
    .await;
    let test = spine_test_codex()
        .with_model_info_override("gpt-5.4", |model_info| {
            model_info.use_responses_lite = true;
        })
        .with_config(|config| {
            config
                .features
                .enable(Feature::CodeMode)
                .expect("enable CodeMode");
        })
        .build(&server)
        .await?;

    test.submit_turn("inspect Spine tools").await?;

    let body = response_mock.single_request().body_json();
    let tools = additional_tools(&body)?;
    for tool_name in ["open", "close", "next"] {
        assert!(has_namespaced_tool(&tools, "spine", tool_name));
    }
    let rendered_tools = serde_json::to_string(&tools)?;
    assert!(!rendered_tools.contains("spine__open"));
    assert!(!rendered_tools.contains("spine__close"));
    assert!(!rendered_tools.contains("spine__next"));

    let additional_items = body["input"]
        .as_array()
        .context("Responses request input should be an array")?
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("additional_tools"))
        .collect::<Vec<_>>();
    let spine_item = additional_items
        .iter()
        .copied()
        .find(|item| {
            item["tools"].as_array().is_some_and(|tools| {
                has_namespaced_tool(tools, spine_core::host::SPINE_NAMESPACE, "open")
            })
        })
        .context("Spine tools should have their own bounded AdditionalTools item")?;
    assert_eq!(additional_items.last().copied(), Some(spine_item));
    assert!(
        serde_json::to_vec(&serde_json::json!({ "input": [spine_item] }))?.len()
            <= spine_core::host::MAX_MODEL_VISIBLE_PROVIDER_VALUE_BYTES
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn feature_off_same_named_namespace_stays_in_the_base_tools_item() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let response_mock = responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_response_created("resp-feature-off-same-name"),
            responses::ev_completed("resp-feature-off-same-name"),
        ]),
    )
    .await;
    let base_test = test_codex()
        .with_model_info_override("gpt-5.4", |model_info| {
            model_info.use_responses_lite = true;
        })
        .build_with_auto_env(&server)
        .await?;
    let dynamic_tool = DynamicToolSpec::Namespace(DynamicToolNamespaceSpec {
        name: spine_core::host::SPINE_NAMESPACE.to_string(),
        description: "Unrelated client-owned tools.".to_string(),
        tools: vec![DynamicToolNamespaceTool::Function(
            DynamicToolFunctionSpec {
                name: "unrelated".to_string(),
                description: "A non-Spine tool in a same-named namespace.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false,
                }),
                defer_loading: false,
            },
        )],
    });
    let new_thread = base_test
        .thread_manager
        .start_thread(StartThreadOptions {
            dynamic_tools: vec![dynamic_tool],
            ..StartThreadOptions::new(base_test.config.clone())
        })
        .await?;
    let mut test = base_test;
    test.codex = new_thread.thread;
    test.session_configured = new_thread.session_configured;

    test.submit_turn("inspect unrelated same-named tools")
        .await?;

    let body = response_mock.single_request().body_json();
    let additional_items = body["input"]
        .as_array()
        .context("Responses request input should be an array")?
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("additional_tools"))
        .collect::<Vec<_>>();
    assert_eq!(additional_items.len(), 1);
    let tools = additional_items[0]["tools"]
        .as_array()
        .context("Base AdditionalTools item should contain tools")?;
    assert!(has_namespaced_tool(
        tools,
        spine_core::host::SPINE_NAMESPACE,
        "unrelated"
    ));
    Ok(())
}
