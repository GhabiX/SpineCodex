use anyhow::Context;
use anyhow::Result;
use codex_core::test_support::submit_interrupt_then_mailbox_for_test;
use codex_features::Feature;
use codex_protocol::AgentPath;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ToolMode;
use codex_protocol::protocol::AgentStatus;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::protocol::MULTI_AGENT_MODE_OPEN_TAG;
use codex_protocol::protocol::MultiAgentVersion;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::RolloutLine;
use codex_protocol::user_input::UserInput;
use codex_spine_core::SPINE_SPAWN_RESULT_SCHEMA;
use codex_spine_core::SpawnReceipt;
use core_test_support::responses::ResponseMock;
use core_test_support::responses::ResponsesRequest;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_custom_tool_call;
use core_test_support::responses::ev_function_call_with_namespace;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::ev_shell_command_call;
use core_test_support::responses::mount_response_once_match;
use core_test_support::responses::mount_sse_once_match;
use core_test_support::responses::sse;
use core_test_support::responses::sse_response;
use core_test_support::responses::start_mock_server;
use core_test_support::test_codex::TestCodex;
use core_test_support::test_codex::TestCodexBuilder;
use core_test_support::test_codex::spine_test_codex;
use core_test_support::wait_for_event;
use serde_json::Value;
use serde_json::json;
use std::time::Duration;
use tokio::time::Instant;
use tokio::time::sleep;
use wiremock::ResponseTemplate;

const SPAWN_NAMESPACE: &str = "spine";
const SPAWN_TOOL: &str = "spawn";
const SPAWN_CALL_ID: &str = "spawn-lifecycle-call";
const FIRST_PARENT_PROMPT: &str = "run the lifecycle spawn batch";
const SECOND_PARENT_PROMPT: &str = "run the replacement spawn batch";
const BRANCH_PROMPT_MARKER: &str = "You are a spawned execution branch.";
const CORRECTION_MESSAGE: &str = concat!(
    "This spawned execution branch remains active. Continue exactly the declared assignment and ",
    "follow its collaboration contract when one is declared. When the assignment is complete or ",
    "precisely bounded, return exactly one non-empty, tool-free assistant final response containing ",
    "terminal memory. That response ends this branch execution."
);
const CODE_MODE_SPINE_CARRIER_MARKER: &str = "spine.code_mode.output.v1";

fn body_contains(request: &wiremock::Request, text: &str) -> bool {
    decoded_body(request)
        .and_then(|body| serde_json::from_slice::<Value>(&body).ok())
        .is_some_and(|body| body.to_string().contains(text))
}

fn child_task_marker(request: &wiremock::Request, marker: &str) -> bool {
    decoded_body(request)
        .and_then(|body| serde_json::from_slice::<Value>(&body).ok())
        .is_some_and(|body| body_has_child_task_marker(&body, marker))
}

fn body_has_child_task_marker(body: &Value, marker: &str) -> bool {
    body.get("input")
        .and_then(Value::as_array)
        .is_some_and(|items| {
            items.iter().any(|item| {
                item.get("type").and_then(Value::as_str) == Some("message")
                    && item.get("role").and_then(Value::as_str) == Some("user")
                    && item
                        .get("content")
                        .and_then(Value::as_array)
                        .is_some_and(|content| {
                            content.iter().any(|part| {
                                part.get("text")
                                    .and_then(Value::as_str)
                                    .is_some_and(|text| {
                                        text.contains(BRANCH_PROMPT_MARKER) && text.contains(marker)
                                    })
                            })
                        })
            })
        })
}

fn has_function_call_output(request: &wiremock::Request, call_id: &str) -> bool {
    decoded_body(request)
        .and_then(|body| serde_json::from_slice::<Value>(&body).ok())
        .is_some_and(|body| {
            body.get("input")
                .and_then(Value::as_array)
                .is_some_and(|items| {
                    items.iter().any(|item| {
                        item.get("type").and_then(Value::as_str) == Some("function_call_output")
                            && item.get("call_id").and_then(Value::as_str) == Some(call_id)
                    })
                })
        })
}

fn is_parent_spawn_request(request: &wiremock::Request) -> bool {
    body_contains(request, FIRST_PARENT_PROMPT)
        && !body_contains(request, BRANCH_PROMPT_MARKER)
        && !has_function_call_output(request, SPAWN_CALL_ID)
}

fn decoded_body(request: &wiremock::Request) -> Option<Vec<u8>> {
    let is_zstd = request
        .headers
        .get("content-encoding")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .any(|entry| entry.trim().eq_ignore_ascii_case("zstd"))
        });
    if is_zstd {
        zstd::stream::decode_all(std::io::Cursor::new(&request.body)).ok()
    } else {
        Some(request.body.clone())
    }
}

fn persisted_function_call_output(test: &TestCodex, call_id: &str) -> Result<String> {
    let path = test
        .codex
        .rollout_path()
        .context("test thread is missing its rollout path")?;
    let rollout = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read rollout {}", path.display()))?;
    rollout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(serde_json::from_str::<RolloutLine>)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .find_map(|line| match line.item {
            RolloutItem::ResponseItem(ResponseItem::FunctionCallOutput {
                call_id: output_call_id,
                output,
                ..
            }) if output_call_id == call_id => output.body.to_text(),
            _ => None,
        })
        .with_context(|| format!("rollout is missing function output for `{call_id}`"))
}

fn spawn_args_for(tasks: &[(&str, &str)]) -> String {
    let tasks = tasks
        .iter()
        .map(|(summary, prompt)| json!({"summary": summary, "prompt": prompt}))
        .collect::<Vec<_>>();
    json!({"tasks": tasks}).to_string()
}

fn spawn_args(first_marker: &str, second_marker: &str) -> String {
    spawn_args_for(&[("first", first_marker), ("second", second_marker)])
}

fn spine_builder() -> TestCodexBuilder {
    spine_test_codex()
        .with_spine_spawn()
        .with_model("koffing")
        .with_config(|config| {
            config.spine_spawn.max_concurrent_threads_per_session = 3;
            config.multi_agent_v2.max_concurrent_threads_per_session = 17;
            config.model_provider.request_max_retries = Some(0);
            config.model_provider.stream_max_retries = Some(0);
            config.model_provider.supports_websockets = false;
        })
}

fn metadata_v2_spine_builder() -> TestCodexBuilder {
    spine_builder()
        .with_model_info_override("gpt-5.6-sol", |model_info| {
            model_info.multi_agent_version = Some(MultiAgentVersion::V2);
        })
        .with_config(|config| {
            config.multi_agent_v2.root_agent_usage_hint_text =
                Some("metadata-v2-root-usage-hint".to_string());
            config.multi_agent_v2.subagent_usage_hint_text =
                Some("metadata-v2-subagent-usage-hint".to_string());
        })
}

fn multi_agent_v2_spine_builder() -> TestCodexBuilder {
    metadata_v2_spine_builder().with_config(|config| {
        config
            .features
            .enable(Feature::MultiAgentV2)
            .expect("enable MultiAgentV2");
    })
}

fn nested_spawn_builder() -> TestCodexBuilder {
    metadata_v2_spine_builder()
        .with_model_info_override("gpt-5.6-sol", |model_info| {
            model_info.use_responses_lite = true;
            model_info.tool_mode = Some(ToolMode::CodeModeOnly);
            model_info
                .experimental_supported_tools
                .push("test_sync_tool".to_string());
        })
        .with_config(|config| {
            config
                .features
                .enable(Feature::CodeMode)
                .expect("enable CodeMode");
        })
}

async fn wait_for_request(
    mock_response: &ResponseMock,
    label: &str,
    predicate: impl Fn(&core_test_support::responses::ResponsesRequest) -> bool,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if mock_response.requests().iter().any(&predicate) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for mocked Responses request `{label}`");
        }
        sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_for_code_mode_first_output(test: &TestCodex, outer_exec_call_id: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if codex_core::test_support::code_mode_is_waiting_for_first_output(
            &test.codex,
            outer_exec_call_id,
        ) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            anyhow::bail!(
                "timed out waiting for Code Mode first-output settlement `{outer_exec_call_id}`"
            );
        }
        sleep(Duration::from_millis(10)).await;
    }
}

fn parent_projection_request(
    mock_response: &ResponseMock,
    first_memory: &str,
    second_memory: &str,
) -> core_test_support::responses::ResponsesRequest {
    mock_response
        .requests()
        .into_iter()
        .find(|request| {
            request.body_contains_text(first_memory)
                && request.body_contains_text(second_memory)
                && !request.body_contains_text(BRANCH_PROMPT_MARKER)
        })
        .expect("parent follow-up should contain the completed spawn projection")
}

fn unique_matching_request(
    mock_response: &ResponseMock,
    label: &str,
    predicate: impl Fn(&ResponsesRequest) -> bool,
) -> ResponsesRequest {
    let mut matches = mock_response
        .requests()
        .into_iter()
        .filter(predicate)
        .collect::<Vec<_>>();
    assert_eq!(
        matches.len(),
        1,
        "unique mocked request `{label}`: {}",
        matches
            .iter()
            .map(|request| request.body_json().to_string())
            .collect::<Vec<_>>()
            .join("\n---\n")
    );
    matches.remove(0)
}

fn first_matching_request(
    mock_response: &ResponseMock,
    predicate: impl Fn(&ResponsesRequest) -> bool,
) -> ResponsesRequest {
    mock_response
        .requests()
        .into_iter()
        .find(predicate)
        .expect("matching mocked request")
}

fn has_namespace(request: &ResponsesRequest, namespace: &str) -> bool {
    request
        .body_json()
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| {
            tools.iter().any(|tool| {
                tool.get("type").and_then(Value::as_str) == Some("namespace")
                    && tool.get("name").and_then(Value::as_str) == Some(namespace)
            })
        })
}

async fn build_reverse_completion_fixture(
    first_delay: Duration,
    second_delay: Duration,
) -> Result<(
    wiremock::MockServer,
    TestCodex,
    ResponseMock,
    ResponseMock,
    ResponseMock,
    ResponseMock,
)> {
    let server = start_mock_server().await;
    let parent_spawn = mount_sse_once_match(
        &server,
        is_parent_spawn_request,
        sse(vec![
            ev_response_created("parent-spawn-response"),
            ev_function_call_with_namespace(
                SPAWN_CALL_ID,
                SPAWN_NAMESPACE,
                SPAWN_TOOL,
                &spawn_args("first-child-marker", "second-child-marker"),
            ),
            ev_completed("parent-spawn-response"),
        ]),
    )
    .await;
    let first_child = mount_response_once_match(
        &server,
        |request: &wiremock::Request| child_task_marker(request, "first-child-marker"),
        sse_response(sse(vec![
            ev_response_created("first-child-response"),
            ev_assistant_message("first-child-message", "first memory"),
            ev_completed("first-child-response"),
        ]))
        .set_delay(first_delay),
    )
    .await;
    let second_child = mount_response_once_match(
        &server,
        |request: &wiremock::Request| child_task_marker(request, "second-child-marker"),
        sse_response(sse(vec![
            ev_response_created("second-child-response"),
            ev_assistant_message("second-child-message", "second memory"),
            ev_completed("second-child-response"),
        ]))
        .set_delay(second_delay),
    )
    .await;
    let parent_followup = mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, "first memory")
                && body_contains(request, "second memory")
                && !body_contains(request, BRANCH_PROMPT_MARKER)
        },
        sse(vec![
            ev_response_created("parent-followup-response"),
            ev_assistant_message("parent-followup-message", "parent done"),
            ev_completed("parent-followup-response"),
        ]),
    )
    .await;
    let test = metadata_v2_spine_builder().build(&server).await?;
    assert!(test.config.features.enabled(Feature::SpineSpawn));
    assert!(!test.config.features.enabled(Feature::MultiAgentV2));
    let selected_model = test
        .config
        .model_catalog
        .as_ref()
        .and_then(|catalog| {
            catalog
                .models
                .iter()
                .find(|model| model.slug == "gpt-5.6-sol")
        })
        .expect("GPT-5.6-Sol metadata should be present in the test model catalog");
    assert_eq!(
        selected_model.multi_agent_version,
        Some(MultiAgentVersion::V2)
    );
    assert_eq!(
        parent_spawn.requests().len(),
        0,
        "fixture must not issue a request before submit_turn"
    );
    Ok((
        server,
        test,
        parent_spawn,
        first_child,
        second_child,
        parent_followup,
    ))
}

#[test]
fn spine_spawn_runs_with_metadata_v2_and_multi_agent_feature_off() -> Result<()> {
    const TEST_STACK_SIZE_BYTES: usize = 16 * 1024 * 1024;

    let handle = std::thread::Builder::new()
        .name("spine_spawn_prefix_trim".to_string())
        .stack_size(TEST_STACK_SIZE_BYTES)
        .spawn(|| -> Result<()> {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                .thread_stack_size(TEST_STACK_SIZE_BYTES)
                .enable_all()
                .build()?;
            runtime.block_on(spawn_starts_batch_concurrently_and_orders_reverse_completion_impl())
        })?;

    match handle.join() {
        Ok(result) => result,
        Err(_) => Err(anyhow::anyhow!("spine.spawn prefix test thread panicked")),
    }
}

async fn spawn_starts_batch_concurrently_and_orders_reverse_completion_impl() -> Result<()> {
    let (server, test, parent_spawn, first_child, second_child, parent_followup) =
        build_reverse_completion_fixture(Duration::from_millis(500), Duration::from_millis(100))
            .await?;

    let observe_overlap = async {
        if let Err(error) = wait_for_request(&first_child, "first child", |request| {
            request.body_contains_text("first-child-marker")
                && request.body_contains_text(BRANCH_PROMPT_MARKER)
        })
        .await
        {
            let requests = server.received_requests().await.unwrap_or_default();
            let request_bodies = requests
                .iter()
                .filter_map(decoded_body)
                .filter_map(|body| String::from_utf8(body).ok())
                .collect::<Vec<_>>();
            anyhow::bail!(
                "{error}; parent requests: {}; parent tool output: {:?}; received: {} {:?}",
                parent_spawn.requests().len(),
                parent_followup.function_call_output_text(SPAWN_CALL_ID),
                requests.len(),
                request_bodies,
            );
        }
        wait_for_request(&second_child, "second child", |request| {
            request.body_contains_text("second-child-marker")
                && request.body_contains_text(BRANCH_PROMPT_MARKER)
        })
        .await?;
        assert!(
            parent_followup
                .requests()
                .iter()
                .all(|request| !request.body_contains_text("first memory")),
            "parent must not publish a receipt while the slower child is running"
        );
        assert_eq!(
            test.thread_manager.list_thread_ids().await.len(),
            3,
            "root plus both transaction children must be live together"
        );
        Result::<()>::Ok(())
    };
    tokio::try_join!(test.submit_turn(FIRST_PARENT_PROMPT), observe_overlap)?;

    let parent_request =
        parent_projection_request(&parent_followup, "first memory", "second memory");
    let rendered = parent_request.body_json().to_string();
    let first_position = rendered.find("first memory");
    let second_position = rendered.find("second memory");
    assert!(
        first_position < second_position,
        "parent projection must preserve task ordinal order: first={first_position:?}, second={second_position:?}, request={rendered}"
    );

    let parent_first_request =
        unique_matching_request(&parent_spawn, "initial parent", |request| {
            request.body_contains_text(FIRST_PARENT_PROMPT)
                && !request.body_contains_text(BRANCH_PROMPT_MARKER)
                && request.function_call_output_text(SPAWN_CALL_ID).is_none()
        });
    let child_first_request = first_matching_request(&first_child, |request| {
        request.body_contains_text("first-child-marker")
            && request.body_contains_text(BRANCH_PROMPT_MARKER)
    });
    let parent_first_body = parent_first_request.body_json();
    let child_first_body = child_first_request.body_json();
    assert!(!has_namespace(&parent_first_request, "collaboration"));
    assert!(!has_namespace(&child_first_request, "collaboration"));
    for request in [&parent_first_request, &child_first_request] {
        assert!(!request.body_contains_text("metadata-v2-root-usage-hint"));
        assert!(!request.body_contains_text("metadata-v2-subagent-usage-hint"));
        assert!(!request.body_contains_text(MULTI_AGENT_MODE_OPEN_TAG));
    }
    assert!(
        child_first_request.body_contains_text(FIRST_PARENT_PROMPT),
        "FullHistory child must retain semantic access to the parent turn"
    );
    assert!(
        child_first_request.body_contains_text("first-child-marker"),
        "child task envelope must be appended to the inherited history"
    );
    let child_input = child_first_body["input"]
        .as_array()
        .expect("child request input must be an array");
    assert!(
        !child_input
            .iter()
            .any(|item| { item.get("call_id").and_then(Value::as_str) == Some(SPAWN_CALL_ID) }),
        "child must not inherit the current spine.spawn request or synthetic output"
    );
    let parent_input = parent_first_body["input"]
        .as_array()
        .expect("parent request input must be an array");
    let exact_lcp = parent_input
        .iter()
        .zip(child_input)
        .take_while(|(parent, child)| parent == child)
        .count();
    assert_eq!(
        exact_lcp,
        parent_input.len(),
        "child must preserve the complete parent request input as an exact prefix"
    );
    let parent_cache_key = parent_first_body["prompt_cache_key"]
        .as_str()
        .expect("parent request must expose prompt_cache_key")
        .to_string();
    let child_cache_key = child_first_body["prompt_cache_key"]
        .as_str()
        .expect("child request must expose prompt_cache_key")
        .to_string();
    assert_eq!(
        parent_cache_key, child_cache_key,
        "Spine spawn child must share the parent's prompt cache affinity"
    );
    eprintln!(
        "SPINE_SPAWN_CONTEXT_DIAGNOSTIC {}",
        json!({
            "semantic_parent_prompt": true,
            "parent_input_items": parent_input.len(),
            "child_input_items": child_input.len(),
            "exact_lcp_items": exact_lcp,
            "parent_prompt_cache_key": parent_cache_key,
            "child_prompt_cache_key": child_cache_key,
            "cache_affinity_shared": parent_cache_key == child_cache_key,
            "parent_request_prefix_exact": exact_lcp == parent_input.len(),
            "inherited_in_flight_spawn_call": false,
            "provider_cache_hit_claim": false,
        })
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_child_salvage_preserves_memory_and_cache_key() -> Result<()> {
    let server = start_mock_server().await;
    let parent_prompt = "run a spawn batch with failure salvage";
    mount_sse_once_match(
        &server,
        move |request: &wiremock::Request| {
            body_contains(request, parent_prompt) && !body_contains(request, BRANCH_PROMPT_MARKER)
        },
        sse(vec![
            ev_response_created("salvage-parent-response"),
            ev_function_call_with_namespace(
                SPAWN_CALL_ID,
                SPAWN_NAMESPACE,
                SPAWN_TOOL,
                &spawn_args("salvage-first-child-marker", "salvage-second-child-marker"),
            ),
            ev_completed("salvage-parent-response"),
        ]),
    )
    .await;
    mount_response_once_match(
        &server,
        |request: &wiremock::Request| {
            child_task_marker(request, "salvage-first-child-marker")
                && !body_contains(request, "failure-diagnostic")
        },
        ResponseTemplate::new(503).set_body_json(json!({
            "error": {
                "code": "server_is_overloaded",
                "message": "selected model is at capacity"
            }
        })),
    )
    .await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            child_task_marker(request, "salvage-first-child-marker")
                && body_contains(request, "failure-diagnostic")
        },
        sse(vec![
            ev_response_created("salvage-memory-response"),
            ev_assistant_message(
                "salvage-memory-message",
                "confirmed progress survived the upstream failure",
            ),
            ev_completed("salvage-memory-response"),
        ]),
    )
    .await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| child_task_marker(request, "salvage-second-child-marker"),
        sse(vec![
            ev_response_created("salvage-second-response"),
            ev_assistant_message("salvage-second-message", "second child completed"),
            ev_completed("salvage-second-response"),
        ]),
    )
    .await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            !body_contains(request, BRANCH_PROMPT_MARKER)
                && body_contains(request, "confirmed progress survived the upstream failure")
                && body_contains(request, "child errored")
        },
        sse(vec![
            ev_response_created("salvage-parent-followup"),
            ev_assistant_message("salvage-parent-final", "failure salvage observed"),
            ev_completed("salvage-parent-followup"),
        ]),
    )
    .await;

    let test = spine_builder().build(&server).await?;
    test.submit_turn(parent_prompt).await?;

    let requests = server.received_requests().await.unwrap_or_default();
    let failed_requests = requests
        .iter()
        .filter(|request| {
            child_task_marker(request, "salvage-first-child-marker")
                && !body_contains(request, "failure-diagnostic")
        })
        .collect::<Vec<_>>();
    let salvage_requests = requests
        .iter()
        .filter(|request| {
            child_task_marker(request, "salvage-first-child-marker")
                && body_contains(request, "failure-diagnostic")
        })
        .collect::<Vec<_>>();
    let second_child_requests = requests
        .iter()
        .filter(|request| child_task_marker(request, "salvage-second-child-marker"))
        .collect::<Vec<_>>();
    let parent_followup_requests = requests
        .iter()
        .filter(|request| {
            !body_contains(request, BRANCH_PROMPT_MARKER)
                && body_contains(request, "confirmed progress survived the upstream failure")
                && body_contains(request, "child errored")
        })
        .collect::<Vec<_>>();
    assert_eq!(failed_requests.len(), 1);
    assert_eq!(salvage_requests.len(), 1);
    assert_eq!(second_child_requests.len(), 1);
    assert_eq!(parent_followup_requests.len(), 1);

    let failed_request = failed_requests[0];
    let salvage_request = salvage_requests[0];
    let failed_body: Value =
        serde_json::from_slice(&decoded_body(failed_request).expect("failed request body"))?;
    let salvage_body: Value =
        serde_json::from_slice(&decoded_body(salvage_request).expect("salvage request body"))?;
    assert_eq!(
        failed_body["prompt_cache_key"], salvage_body["prompt_cache_key"],
        "salvage must retain the failed child cache key"
    );
    assert_eq!(salvage_body["tool_choice"], "none");
    assert_eq!(failed_body["instructions"], salvage_body["instructions"]);
    assert_eq!(failed_body["tools"], salvage_body["tools"]);
    let failed_input = failed_body["input"].as_array().expect("failed input array");
    let salvage_input = salvage_body["input"]
        .as_array()
        .expect("salvage input array");
    assert_eq!(
        &salvage_input[..failed_input.len()],
        failed_input.as_slice(),
        "salvage must preserve the failed request input as an exact prefix"
    );
    assert_eq!(salvage_input.len(), failed_input.len() + 1);
    assert_eq!(
        salvage_input.last().and_then(|item| item["role"].as_str()),
        Some("developer")
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spawn_progress_reports_running_without_child_spine_nodes() -> Result<()> {
    let server = start_mock_server().await;
    mount_sse_once_match(
        &server,
        is_parent_spawn_request,
        sse(vec![
            ev_response_created("running-progress-parent-spawn"),
            ev_function_call_with_namespace(
                SPAWN_CALL_ID,
                SPAWN_NAMESPACE,
                SPAWN_TOOL,
                &spawn_args("running-progress-first", "running-progress-second"),
            ),
            ev_completed("running-progress-parent-spawn"),
        ]),
    )
    .await;
    mount_response_once_match(
        &server,
        |request: &wiremock::Request| child_task_marker(request, "running-progress-first"),
        sse_response(sse(vec![
            ev_response_created("running-progress-first-response"),
            ev_assistant_message("running-progress-first-message", "first memory"),
            ev_completed("running-progress-first-response"),
        ]))
        .set_delay(Duration::from_millis(300)),
    )
    .await;
    mount_response_once_match(
        &server,
        |request: &wiremock::Request| child_task_marker(request, "running-progress-second"),
        sse_response(sse(vec![
            ev_response_created("running-progress-second-response"),
            ev_assistant_message("running-progress-second-message", "second memory"),
            ev_completed("running-progress-second-response"),
        ]))
        .set_delay(Duration::from_millis(300)),
    )
    .await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, "first memory")
                && body_contains(request, "second memory")
                && !body_contains(request, "You are one branch of a spine.spawn fission")
        },
        sse(vec![
            ev_response_created("running-progress-parent-followup"),
            ev_assistant_message("running-progress-parent-message", "parent done"),
            ev_completed("running-progress-parent-followup"),
        ]),
    )
    .await;
    let test = spine_builder().build(&server).await?;

    test.codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: FIRST_PARENT_PROMPT.to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: Default::default(),
        })
        .await?;

    let mut phases = [0_u8; 2];
    let mut saw_running = [false; 2];
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), test.codex.next_event())
            .await
            .context("timed out waiting for spawn progress")??;
        match event.msg {
            EventMsg::SpineSpawnProgress(progress) if progress.call_id == SPAWN_CALL_ID => {
                for task in progress.tasks {
                    let ordinal = usize::try_from(task.ordinal).context("task ordinal overflow")?;
                    let phase = match task.status {
                        AgentStatus::PendingInit => 0,
                        AgentStatus::Running => {
                            saw_running[ordinal] = true;
                            1
                        }
                        _ => {
                            assert!(
                                saw_running[ordinal],
                                "task {ordinal} reached {status:?} without reporting Running",
                                status = task.status
                            );
                            2
                        }
                    };
                    assert!(
                        phase >= phases[ordinal],
                        "task {ordinal} progress regressed from phase {previous} to {phase}",
                        previous = phases[ordinal]
                    );
                    phases[ordinal] = phase;
                }
            }
            EventMsg::TurnComplete(_) => break,
            _ => {}
        }
    }

    assert_eq!((phases, saw_running), ([2, 2], [true, true]));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn responses_lite_exec_batches_ordinary_tools_with_nested_spine_spawn() -> Result<()> {
    let server = start_mock_server().await;
    let parent_prompt = "batch ordinary tools with nested Spine spawn";
    let code = r#"// @exec: {"yield_time_ms": 30000}
const syncArgs = () => ({
  sleep_after_ms: 50,
  barrier: {
    id: "spine-code-mode-parallel-spawn",
    participants: 2,
    timeout_ms: 10_000,
  },
});
const [left, right, spawned] = await Promise.all([
  tools.test_sync_tool(syncArgs()),
  tools.test_sync_tool(syncArgs()),
  tools.spine__spawn({
    tasks: [
      {summary: "first", prompt: "nested-first-child-marker"},
      {summary: "second", prompt: "nested-second-child-marker"},
    ],
  }),
]);
text(JSON.stringify({left, right, spawned}));
"#;
    let parent_exec = mount_sse_once_match(
        &server,
        move |request: &wiremock::Request| {
            body_contains(request, parent_prompt)
                && !body_contains(request, BRANCH_PROMPT_MARKER)
                && !body_contains(request, "exec-nested-spawn")
        },
        sse(vec![
            ev_response_created("nested-spawn-parent-response"),
            ev_custom_tool_call("exec-nested-spawn", "exec", code),
            ev_completed("nested-spawn-parent-response"),
        ]),
    )
    .await;
    let first_child = mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            child_task_marker(request, "nested-first-child-marker")
                && !body_contains(request, "exec-nested-spawn")
        },
        sse(vec![
            ev_response_created("nested-spawn-first-response"),
            ev_assistant_message("nested-spawn-first-message", "nested first memory"),
            ev_completed("nested-spawn-first-response"),
        ]),
    )
    .await;
    let second_child = mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            child_task_marker(request, "nested-second-child-marker")
                && !body_contains(request, "exec-nested-spawn")
        },
        sse(vec![
            ev_response_created("nested-spawn-second-response"),
            ev_assistant_message("nested-spawn-second-message", "nested second memory"),
            ev_completed("nested-spawn-second-response"),
        ]),
    )
    .await;
    let parent_followup = mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, "nested first memory")
                && body_contains(request, "nested second memory")
                && !body_contains(request, BRANCH_PROMPT_MARKER)
        },
        sse(vec![
            ev_response_created("nested-spawn-followup-response"),
            ev_assistant_message("nested-spawn-followup-message", "nested parent done"),
            ev_completed("nested-spawn-followup-response"),
        ]),
    )
    .await;

    let mut builder = nested_spawn_builder();
    let test = builder.build(&server).await?;

    test.submit_turn(parent_prompt).await?;
    test.codex.flush_rollout().await?;

    assert_eq!(parent_exec.requests().len(), 1);
    let first_child_requests = first_child
        .requests()
        .into_iter()
        .filter(|request| {
            body_has_child_task_marker(&request.body_json(), "nested-first-child-marker")
        })
        .collect::<Vec<_>>();
    let first_child_request_ids = first_child_requests
        .iter()
        .map(|request| {
            let body = request.body_json();
            (
                body["client_metadata"]["thread_id"].clone(),
                body["client_metadata"]["turn_id"].clone(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        first_child_requests.len(),
        1,
        "unexpected first child requests: {first_child_request_ids:?}"
    );
    let second_child_requests = second_child
        .requests()
        .into_iter()
        .filter(|request| {
            body_has_child_task_marker(&request.body_json(), "nested-second-child-marker")
        })
        .collect::<Vec<_>>();
    assert_eq!(second_child_requests.len(), 1);
    assert_eq!(parent_followup.requests().len(), 1);

    for child_request in first_child_requests.iter().chain(&second_child_requests) {
        assert!(child_request.body_contains_text(parent_prompt));
        assert!(
            !child_request.body_contains_text("spine-code-mode-parallel-spawn"),
            "child history must stop before the outer exec request: {}",
            child_request.body_json()
        );
    }

    let followup = parent_followup.single_request();
    let visible_output = followup
        .custom_tool_call_output("exec-nested-spawn")
        .get("output")
        .and_then(Value::as_array)
        .context("exec output should preserve content items")?
        .iter()
        .filter_map(|item| item.get("text").and_then(Value::as_str))
        .collect::<String>();
    assert!(
        visible_output.contains(r#""left":"ok""#),
        "{visible_output}"
    );
    assert!(
        visible_output.contains(r#""right":"ok""#),
        "{visible_output}"
    );
    assert!(
        visible_output.contains(r#""spawned":"Spine spawn accepted.""#),
        "{visible_output}"
    );
    let followup_body = followup.body_json().to_string();
    assert!(!followup_body.contains(CODE_MODE_SPINE_CARRIER_MARKER));
    assert!(!followup_body.contains(SPINE_SPAWN_RESULT_SCHEMA));
    assert!(followup_body.contains("nested first memory"));
    assert!(followup_body.contains("nested second memory"));

    let rollout_path = test.codex.rollout_path().context("rollout path")?;
    let rollout = std::fs::read_to_string(&rollout_path)?;
    let carrier = rollout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(serde_json::from_str::<RolloutLine>)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .find_map(|line| match line.item {
            RolloutItem::ResponseItem(ResponseItem::CustomToolCallOutput {
                call_id,
                name,
                output,
                ..
            }) if call_id == "exec-nested-spawn"
                && name.as_deref() == Some(CODE_MODE_SPINE_CARRIER_MARKER) =>
            {
                output.body.to_text()
            }
            _ => None,
        })
        .context("raw outer exec output should contain the marked carrier")?;
    let carrier: Value = serde_json::from_str(&carrier)?;
    let calls = carrier["nested_spine_calls"]
        .as_array()
        .context("carrier nested calls")?;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["invocation_ordinal"], 0);
    assert_eq!(calls[0]["name"], "spawn");
    assert_eq!(calls[0]["output"]["success"], true);
    let receipt: SpawnReceipt = serde_json::from_str(
        calls[0]["output"]["body"]
            .as_str()
            .context("nested spawn receipt body")?,
    )?;
    assert_eq!(receipt.schema, SPINE_SPAWN_RESULT_SCHEMA);
    assert_eq!(receipt.results.len(), 2);
    assert_eq!(receipt.results[0].memory_body, "nested first memory");
    assert_eq!(receipt.results[1].memory_body, "nested second memory");

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interrupting_nested_spawn_tears_down_children() -> Result<()> {
    let server = start_mock_server().await;
    let parent_prompt = "interrupt nested Spine spawn";
    let code = r#"// @exec: {"yield_time_ms": 100}
await tools.spine__spawn({
  tasks: [
    {summary: "first", prompt: "nested-cancel-first-marker"},
    {summary: "second", prompt: "nested-cancel-second-marker"},
  ],
});
"#;
    mount_sse_once_match(
        &server,
        move |request: &wiremock::Request| {
            body_contains(request, parent_prompt) && !body_contains(request, BRANCH_PROMPT_MARKER)
        },
        sse(vec![
            ev_response_created("nested-cancel-parent-response"),
            ev_custom_tool_call("exec-nested-cancel", "exec", code),
            ev_completed("nested-cancel-parent-response"),
        ]),
    )
    .await;
    let first_child = mount_response_once_match(
        &server,
        |request: &wiremock::Request| child_task_marker(request, "nested-cancel-first-marker"),
        sse_response(sse(vec![
            ev_response_created("nested-cancel-first-response"),
            ev_assistant_message("nested-cancel-first-message", "too late"),
            ev_completed("nested-cancel-first-response"),
        ]))
        .set_delay(Duration::from_secs(30)),
    )
    .await;
    let second_child = mount_response_once_match(
        &server,
        |request: &wiremock::Request| child_task_marker(request, "nested-cancel-second-marker"),
        sse_response(sse(vec![
            ev_response_created("nested-cancel-second-response"),
            ev_assistant_message("nested-cancel-second-message", "too late"),
            ev_completed("nested-cancel-second-response"),
        ]))
        .set_delay(Duration::from_secs(30)),
    )
    .await;
    let mut builder = nested_spawn_builder();
    let test = builder.build(&server).await?;

    test.codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: parent_prompt.to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: Default::default(),
        })
        .await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnStarted(_))
    })
    .await;
    wait_for_request(&first_child, "nested cancel first child", |request| {
        body_has_child_task_marker(&request.body_json(), "nested-cancel-first-marker")
    })
    .await?;
    wait_for_request(&second_child, "nested cancel second child", |request| {
        body_has_child_task_marker(&request.body_json(), "nested-cancel-second-marker")
    })
    .await?;
    wait_for_code_mode_first_output(&test, "exec-nested-cancel").await?;
    assert_eq!(
        test.thread_manager.list_thread_ids().await.len(),
        3,
        "both nested spawn children must remain active after the exec yield deadline"
    );
    test.codex.submit(Op::Interrupt).await?;

    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnAborted(_))
    })
    .await;
    let ids = test.thread_manager.list_thread_ids().await;
    assert_eq!(
        ids.len(),
        1,
        "parent emitted TurnAborted before nested spawn teardown completed: {ids:?}"
    );
    assert_eq!(
        test.codex.agent_status().await,
        AgentStatus::Interrupted,
        "TurnAborted must leave the parent Interrupted"
    );

    test.codex.flush_rollout().await?;
    let rollout_path = test.codex.rollout_path().context("rollout path")?;
    let rollout = std::fs::read_to_string(&rollout_path)?;
    let persisted_carrier = rollout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(serde_json::from_str::<RolloutLine>)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .any(|line| {
            matches!(
                line.item,
                RolloutItem::ResponseItem(ResponseItem::CustomToolCallOutput {
                    call_id,
                    name,
                    ..
                }) if call_id == "exec-nested-cancel"
                    && name.as_deref() == Some(CODE_MODE_SPINE_CARRIER_MARKER)
            )
        });
    assert!(
        !persisted_carrier,
        "interrupted yielded exec must not persist a Spine carrier"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn intermediate_message_is_corrected_once_and_never_reaches_parent_model() -> Result<()> {
    let server = start_mock_server().await;
    mount_sse_once_match(
        &server,
        is_parent_spawn_request,
        sse(vec![
            ev_response_created("parent-spawn-response"),
            ev_function_call_with_namespace(
                SPAWN_CALL_ID,
                SPAWN_NAMESPACE,
                SPAWN_TOOL,
                &spawn_args("corrected-child-marker", "ordinary-child-marker"),
            ),
            ev_completed("parent-spawn-response"),
        ]),
    )
    .await;
    let corrected_child = mount_response_once_match(
        &server,
        |request: &wiremock::Request| child_task_marker(request, "corrected-child-marker"),
        sse_response(sse(vec![
            ev_response_created("corrected-child-first-response"),
            ev_shell_command_call("child-yield-call", "true"),
            ev_completed("corrected-child-first-response"),
        ]))
        .set_delay(Duration::from_millis(300)),
    )
    .await;
    let corrected_child_followup = mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            has_function_call_output(request, "child-yield-call")
                && body_contains(request, CORRECTION_MESSAGE)
        },
        sse(vec![
            ev_response_created("corrected-child-final-response"),
            ev_assistant_message("corrected-child-final-message", "corrected child memory"),
            ev_completed("corrected-child-final-response"),
        ]),
    )
    .await;
    mount_response_once_match(
        &server,
        |request: &wiremock::Request| child_task_marker(request, "ordinary-child-marker"),
        sse_response(sse(vec![
            ev_response_created("ordinary-child-response"),
            ev_assistant_message("ordinary-child-message", "ordinary child memory"),
            ev_completed("ordinary-child-response"),
        ]))
        .set_delay(Duration::from_millis(450)),
    )
    .await;
    let parent_followup = mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, "corrected child memory")
                && body_contains(request, "ordinary child memory")
                && !body_contains(request, BRANCH_PROMPT_MARKER)
        },
        sse(vec![
            ev_response_created("parent-followup-response"),
            ev_assistant_message("parent-followup-message", "parent done"),
            ev_completed("parent-followup-response"),
        ]),
    )
    .await;
    let test = spine_builder().build(&server).await?;

    let inject_intermediate = async {
        wait_for_request(&corrected_child, "corrected child first turn", |request| {
            request.body_contains_text("corrected-child-marker")
                && request.body_contains_text(BRANCH_PROMPT_MARKER)
        })
        .await?;
        test.codex
            .submit(Op::InterAgentCommunication {
                communication: InterAgentCommunication::new(
                    AgentPath::try_from("/root/spawn_spawnlifecyclecall_0")
                        .expect("transaction child path should be valid"),
                    AgentPath::root(),
                    Vec::new(),
                    "intermediate-secret".to_string(),
                    /*trigger_turn*/ false,
                ),
            })
            .await?;
        wait_for_request(
            &corrected_child_followup,
            "corrected child follow-up",
            |request| {
                request.body_contains_text(CORRECTION_MESSAGE)
                    && request.input().iter().any(|item| {
                        item.get("call_id").and_then(Value::as_str) == Some("child-yield-call")
                    })
            },
        )
        .await?;
        Result::<()>::Ok(())
    };
    tokio::try_join!(test.submit_turn(FIRST_PARENT_PROMPT), inject_intermediate)?;

    assert_eq!(
        corrected_child_followup
            .requests()
            .iter()
            .filter(|request| {
                request.body_contains_text(CORRECTION_MESSAGE)
                    && request.input().iter().any(|item| {
                        item.get("call_id").and_then(Value::as_str) == Some("child-yield-call")
                    })
            })
            .count(),
        1
    );
    let parent_request = parent_projection_request(
        &parent_followup,
        "corrected child memory",
        "ordinary child memory",
    );
    assert!(!parent_request.body_contains_text("intermediate-secret"));
    assert!(!parent_request.body_contains_text(CORRECTION_MESSAGE));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn descendant_root_message_is_corrected_while_branch_internal_message_is_delivered()
-> Result<()> {
    const B0_SPAWN_D0_CALL_ID: &str = "b0-spawn-d0";
    const B0_WAIT_CALL_ID: &str = "b0-wait-for-descendant";
    const B0_PATH: &str = "/root/spawn_spawnlifecyclecall_0";
    const D0_SEND_BRANCH_CALL_ID: &str = "d0-send-branch";
    const D0_SEND_ROOT_CALL_ID: &str = "d0-send-root";
    const D0_WAIT_CALL_ID: &str = "d0-wait-for-correction";
    const D0_SECRET: &str = "descendant-intermediate-secret";
    const D0_BRANCH_MESSAGE: &str = "descendant-branch-internal-message";

    let server = start_mock_server().await;
    mount_sse_once_match(
        &server,
        is_parent_spawn_request,
        sse(vec![
            ev_response_created("parent-descendant-spawn-response"),
            ev_function_call_with_namespace(
                SPAWN_CALL_ID,
                SPAWN_NAMESPACE,
                SPAWN_TOOL,
                &spawn_args("descendant-parent-marker", "ordinary-sibling-marker"),
            ),
            ev_completed("parent-descendant-spawn-response"),
        ]),
    )
    .await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            child_task_marker(request, "descendant-parent-marker")
                && !has_function_call_output(request, B0_SPAWN_D0_CALL_ID)
        },
        sse(vec![
            ev_response_created("b0-spawn-d0-response"),
            ev_function_call_with_namespace(
                B0_SPAWN_D0_CALL_ID,
                "collaboration",
                "spawn_agent",
                &json!({
                    "message": "descendant-worker-marker",
                    "task_name": "worker",
                    "fork_turns": "all",
                })
                .to_string(),
            ),
            ev_completed("b0-spawn-d0-response"),
        ]),
    )
    .await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            has_function_call_output(request, B0_SPAWN_D0_CALL_ID)
                && !has_function_call_output(request, B0_WAIT_CALL_ID)
        },
        sse(vec![
            ev_response_created("b0-wait-response"),
            ev_shell_command_call(B0_WAIT_CALL_ID, "sleep 0.3"),
            ev_completed("b0-wait-response"),
        ]),
    )
    .await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            has_function_call_output(request, B0_WAIT_CALL_ID)
                && body_contains(request, D0_BRANCH_MESSAGE)
        },
        sse(vec![
            ev_response_created("b0-final-response"),
            ev_assistant_message("b0-final-message", "descendant branch memory"),
            ev_completed("b0-final-response"),
        ]),
    )
    .await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, "descendant-worker-marker")
                && body_contains(request, "\"type\":\"agent_message\"")
                && !has_function_call_output(request, D0_SEND_ROOT_CALL_ID)
        },
        sse(vec![
            ev_response_created("d0-send-response"),
            ev_function_call_with_namespace(
                D0_SEND_BRANCH_CALL_ID,
                "collaboration",
                "send_message",
                &json!({
                    "target": B0_PATH,
                    "message": D0_BRANCH_MESSAGE,
                })
                .to_string(),
            ),
            ev_function_call_with_namespace(
                D0_SEND_ROOT_CALL_ID,
                "collaboration",
                "send_message",
                &json!({
                    "target": "/root",
                    "message": D0_SECRET,
                })
                .to_string(),
            ),
            ev_shell_command_call(D0_WAIT_CALL_ID, "sleep 0.3"),
            ev_completed("d0-send-response"),
        ]),
    )
    .await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            has_function_call_output(request, D0_SEND_BRANCH_CALL_ID)
                && has_function_call_output(request, D0_SEND_ROOT_CALL_ID)
                && has_function_call_output(request, D0_WAIT_CALL_ID)
                && body_contains(request, CORRECTION_MESSAGE)
        },
        sse(vec![
            ev_response_created("d0-corrected-response"),
            ev_assistant_message("d0-corrected-message", "descendant terminal memory"),
            ev_completed("d0-corrected-response"),
        ]),
    )
    .await;
    mount_response_once_match(
        &server,
        |request: &wiremock::Request| child_task_marker(request, "ordinary-sibling-marker"),
        sse_response(sse(vec![
            ev_response_created("ordinary-sibling-response"),
            ev_assistant_message("ordinary-sibling-message", "ordinary sibling memory"),
            ev_completed("ordinary-sibling-response"),
        ]))
        .set_delay(Duration::from_millis(700)),
    )
    .await;
    let parent_followup = mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, "descendant branch memory")
                && body_contains(request, "ordinary sibling memory")
                && !body_contains(request, BRANCH_PROMPT_MARKER)
        },
        sse(vec![
            ev_response_created("parent-descendant-followup-response"),
            ev_assistant_message("parent-descendant-followup-message", "parent done"),
            ev_completed("parent-descendant-followup-response"),
        ]),
    )
    .await;
    let test = multi_agent_v2_spine_builder().build(&server).await?;
    let mut created_threads = test.thread_manager.subscribe_thread_created();

    test.submit_turn(FIRST_PARENT_PROMPT).await?;

    for _ in 0..3 {
        let thread_id =
            tokio::time::timeout(Duration::from_secs(5), created_threads.recv()).await??;
        assert!(
            test.thread_manager.get_thread(thread_id).await.is_err(),
            "Spawn must remove every direct branch and descendant before returning"
        );
    }
    let parent_request = parent_projection_request(
        &parent_followup,
        "descendant branch memory",
        "ordinary sibling memory",
    );
    assert!(!parent_request.body_contains_text(D0_SECRET));
    assert!(!parent_request.body_contains_text(D0_BRANCH_MESSAGE));
    assert!(!parent_request.body_contains_text(CORRECTION_MESSAGE));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interrupt_tears_down_children_drops_late_mail_and_releases_batch_capacity() -> Result<()> {
    const CANCEL_SPAWN_DESCENDANT_CALL_ID: &str = "cancel-spawn-descendant";
    const LATE_DESCENDANT_MESSAGE: &str = "late-descendant-message-must-not-reach-root";
    let server = start_mock_server().await;
    mount_sse_once_match(
        &server,
        is_parent_spawn_request,
        sse(vec![
            ev_response_created("cancel-parent-response"),
            ev_function_call_with_namespace(
                SPAWN_CALL_ID,
                SPAWN_NAMESPACE,
                SPAWN_TOOL,
                &spawn_args("cancel-first-marker", "cancel-second-marker"),
            ),
            ev_completed("cancel-parent-response"),
        ]),
    )
    .await;
    let cancel_first = mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            child_task_marker(request, "cancel-first-marker")
                && !has_function_call_output(request, CANCEL_SPAWN_DESCENDANT_CALL_ID)
        },
        sse(vec![
            ev_response_created("cancel-first-response"),
            ev_function_call_with_namespace(
                CANCEL_SPAWN_DESCENDANT_CALL_ID,
                "collaboration",
                "spawn_agent",
                &json!({
                    "message": "cancel-descendant-marker",
                    "task_name": "worker",
                    "fork_turns": "all",
                })
                .to_string(),
            ),
            ev_completed("cancel-first-response"),
        ]),
    )
    .await;
    let cancel_first_followup = mount_response_once_match(
        &server,
        |request: &wiremock::Request| {
            has_function_call_output(request, CANCEL_SPAWN_DESCENDANT_CALL_ID)
        },
        sse_response(sse(vec![
            ev_response_created("cancel-first-followup-response"),
            ev_assistant_message("cancel-first-followup-message", "too late"),
            ev_completed("cancel-first-followup-response"),
        ]))
        .set_delay(Duration::from_secs(5)),
    )
    .await;
    let cancel_descendant = mount_response_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, "cancel-descendant-marker")
                && body_contains(request, "\"type\":\"agent_message\"")
        },
        sse_response(sse(vec![
            ev_response_created("cancel-descendant-response"),
            ev_assistant_message("cancel-descendant-message", "too late"),
            ev_completed("cancel-descendant-response"),
        ]))
        .set_delay(Duration::from_secs(5)),
    )
    .await;
    let cancel_second = mount_response_once_match(
        &server,
        |request: &wiremock::Request| child_task_marker(request, "cancel-second-marker"),
        sse_response(sse(vec![
            ev_response_created("cancel-second-response"),
            ev_assistant_message("cancel-second-message", "too late"),
            ev_completed("cancel-second-response"),
        ]))
        .set_delay(Duration::from_secs(5)),
    )
    .await;

    let replacement_call_id = "spawn-replacement-call";
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, SECOND_PARENT_PROMPT)
                && !body_contains(request, LATE_DESCENDANT_MESSAGE)
        },
        sse(vec![
            ev_response_created("replacement-parent-response"),
            ev_function_call_with_namespace(
                replacement_call_id,
                SPAWN_NAMESPACE,
                SPAWN_TOOL,
                &spawn_args("replacement-first-marker", "replacement-second-marker"),
            ),
            ev_completed("replacement-parent-response"),
        ]),
    )
    .await;
    let mut replacement_children = Vec::new();
    for (marker, response, message, memory) in [
        (
            "replacement-first-marker",
            "replacement-first-response",
            "replacement-first-message",
            "replacement first memory",
        ),
        (
            "replacement-second-marker",
            "replacement-second-response",
            "replacement-second-message",
            "replacement second memory",
        ),
    ] {
        replacement_children.push(
            mount_response_once_match(
                &server,
                move |request: &wiremock::Request| child_task_marker(request, marker),
                sse_response(sse(vec![
                    ev_response_created(response),
                    ev_assistant_message(message, memory),
                    ev_completed(response),
                ]))
                .set_delay(Duration::from_secs(5)),
            )
            .await,
        );
    }
    let test = multi_agent_v2_spine_builder().build(&server).await?;
    let mut created_threads = test.thread_manager.subscribe_thread_created();

    test.codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: FIRST_PARENT_PROMPT.to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: Default::default(),
        })
        .await?;
    wait_for_request(&cancel_first, "cancel first child", |request| {
        request.body_contains_text("cancel-first-marker")
            && request.body_contains_text(BRANCH_PROMPT_MARKER)
    })
    .await?;
    wait_for_request(&cancel_second, "cancel second child", |request| {
        request.body_contains_text("cancel-second-marker")
            && request.body_contains_text(BRANCH_PROMPT_MARKER)
    })
    .await?;
    wait_for_request(
        &cancel_first_followup,
        "cancel first child after descendant spawn",
        |request| {
            request.input().iter().any(|item| {
                item.get("call_id").and_then(Value::as_str) == Some(CANCEL_SPAWN_DESCENDANT_CALL_ID)
            })
        },
    )
    .await?;
    wait_for_request(&cancel_descendant, "cancel descendant", |request| {
        request.body_contains_text("cancel-descendant-marker")
    })
    .await?;
    let mut transaction_thread_ids = Vec::new();
    for _ in 0..3 {
        transaction_thread_ids
            .push(tokio::time::timeout(Duration::from_secs(5), created_threads.recv()).await??);
    }
    submit_interrupt_then_mailbox_for_test(
        test.codex.as_ref(),
        InterAgentCommunication::new(
            AgentPath::try_from("/root/spawn_spawnlifecyclecall_0/worker")
                .expect("descendant path should be valid"),
            AgentPath::root(),
            Vec::new(),
            LATE_DESCENDANT_MESSAGE.to_string(),
            /*trigger_turn*/ false,
        ),
    )
    .await?;
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if matches!(test.codex.next_event().await?.msg, EventMsg::TurnAborted(_)) {
                return Result::<()>::Ok(());
            }
        }
    })
    .await
    .context("Interrupt did not complete within the native hard-abort bound")??;

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if test.thread_manager.list_thread_ids().await.len() == 1
            && test.codex.agent_status().await == AgentStatus::Interrupted
        {
            break;
        }
        if Instant::now() >= deadline {
            anyhow::bail!("cancelled transaction children remained loaded");
        }
        sleep(Duration::from_millis(10)).await;
    }
    for thread_id in transaction_thread_ids {
        assert!(
            test.thread_manager.get_thread(thread_id).await.is_err(),
            "Interrupt must remove every direct branch and recursive descendant"
        );
    }

    test.codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: SECOND_PARENT_PROMPT.to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: Default::default(),
        })
        .await?;
    for (mock_response, marker) in replacement_children
        .iter()
        .zip(["replacement-first-marker", "replacement-second-marker"])
    {
        wait_for_request(mock_response, marker, |request| {
            request.body_contains_text(marker) && request.body_contains_text(BRANCH_PROMPT_MARKER)
        })
        .await?;
    }
    assert_eq!(test.thread_manager.list_thread_ids().await.len(), 3);
    test.codex.submit(Op::Interrupt).await?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if test.thread_manager.list_thread_ids().await.len() == 1
            && test.codex.agent_status().await == AgentStatus::Interrupted
        {
            break;
        }
        if Instant::now() >= deadline {
            anyhow::bail!("replacement transaction children remained loaded after cleanup");
        }
        sleep(Duration::from_millis(10)).await;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn successful_batches_release_transaction_children_for_immediate_reuse() -> Result<()> {
    let server = start_mock_server().await;
    let first_call_id = "spawn-first-success-call";
    mount_sse_once_match(
        &server,
        move |request: &wiremock::Request| {
            body_contains(request, FIRST_PARENT_PROMPT)
                && !body_contains(request, SECOND_PARENT_PROMPT)
                && !body_contains(request, BRANCH_PROMPT_MARKER)
                && !has_function_call_output(request, first_call_id)
        },
        sse(vec![
            ev_response_created("first-success-parent-response"),
            ev_function_call_with_namespace(
                first_call_id,
                SPAWN_NAMESPACE,
                SPAWN_TOOL,
                &spawn_args("first-success-a-marker", "first-success-b-marker"),
            ),
            ev_completed("first-success-parent-response"),
        ]),
    )
    .await;
    for (marker, response, message, memory) in [
        (
            "first-success-a-marker",
            "first-success-a-response",
            "first-success-a-message",
            "first batch memory one",
        ),
        (
            "first-success-b-marker",
            "first-success-b-response",
            "first-success-b-message",
            "first batch memory two",
        ),
    ] {
        mount_response_once_match(
            &server,
            move |request: &wiremock::Request| child_task_marker(request, marker),
            sse_response(sse(vec![
                ev_response_created(response),
                ev_assistant_message(message, memory),
                ev_completed(response),
            ])),
        )
        .await;
    }
    let first_followup = mount_sse_once_match(
        &server,
        move |request: &wiremock::Request| {
            body_contains(request, "first batch memory one")
                && body_contains(request, "first batch memory two")
                && !body_contains(request, SECOND_PARENT_PROMPT)
                && has_function_call_output(request, first_call_id)
                && !body_contains(request, BRANCH_PROMPT_MARKER)
        },
        sse(vec![
            ev_response_created("first-success-followup-response"),
            ev_assistant_message("first-success-followup-message", "first batch done"),
            ev_completed("first-success-followup-response"),
        ]),
    )
    .await;

    let second_call_id = "spawn-second-success-call";
    mount_sse_once_match(
        &server,
        move |request: &wiremock::Request| {
            body_contains(request, SECOND_PARENT_PROMPT)
                && !body_contains(request, BRANCH_PROMPT_MARKER)
                && !has_function_call_output(request, second_call_id)
        },
        sse(vec![
            ev_response_created("second-success-parent-response"),
            ev_function_call_with_namespace(
                second_call_id,
                SPAWN_NAMESPACE,
                SPAWN_TOOL,
                &spawn_args("second-success-a-marker", "second-success-b-marker"),
            ),
            ev_completed("second-success-parent-response"),
        ]),
    )
    .await;
    for (marker, response, message, memory) in [
        (
            "second-success-a-marker",
            "second-success-a-response",
            "second-success-a-message",
            "second batch memory one",
        ),
        (
            "second-success-b-marker",
            "second-success-b-response",
            "second-success-b-message",
            "second batch memory two",
        ),
    ] {
        mount_response_once_match(
            &server,
            move |request: &wiremock::Request| child_task_marker(request, marker),
            sse_response(sse(vec![
                ev_response_created(response),
                ev_assistant_message(message, memory),
                ev_completed(response),
            ])),
        )
        .await;
    }
    let second_followup = mount_sse_once_match(
        &server,
        move |request: &wiremock::Request| {
            body_contains(request, "second batch memory one")
                && body_contains(request, "second batch memory two")
                && has_function_call_output(request, second_call_id)
                && !body_contains(request, BRANCH_PROMPT_MARKER)
        },
        sse(vec![
            ev_response_created("second-success-followup-response"),
            ev_assistant_message("second-success-followup-message", "second batch done"),
            ev_completed("second-success-followup-response"),
        ]),
    )
    .await;

    let test = spine_builder().build(&server).await?;
    assert!(!test.config.features.enabled(Feature::MultiAgentV2));

    test.submit_turn(FIRST_PARENT_PROMPT).await?;
    assert_eq!(
        test.thread_manager.list_thread_ids().await.len(),
        1,
        "completed Spine transaction children must be removed before returning the receipt"
    );
    assert!(
        first_followup
            .function_call_output_text(first_call_id)
            .is_some()
    );

    test.submit_turn(SECOND_PARENT_PROMPT).await?;
    assert_eq!(
        test.thread_manager.list_thread_ids().await.len(),
        1,
        "the replacement transaction must release its children too"
    );
    assert!(
        second_followup
            .function_call_output_text(second_call_id)
            .is_some()
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn multiple_spawn_calls_are_rejected_before_child_creation() -> Result<()> {
    const PROMPT: &str = "attempt two spine spawn calls in one response";
    const FIRST_CALL_ID: &str = "duplicate-spawn-first";
    const SECOND_CALL_ID: &str = "duplicate-spawn-second";

    let server = start_mock_server().await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, PROMPT)
                && !has_function_call_output(request, FIRST_CALL_ID)
                && !has_function_call_output(request, SECOND_CALL_ID)
        },
        sse(vec![
            ev_response_created("duplicate-spawn-parent-response"),
            ev_function_call_with_namespace(
                FIRST_CALL_ID,
                SPAWN_NAMESPACE,
                SPAWN_TOOL,
                &spawn_args("duplicate-first-a", "duplicate-first-b"),
            ),
            ev_function_call_with_namespace(
                SECOND_CALL_ID,
                SPAWN_NAMESPACE,
                SPAWN_TOOL,
                &spawn_args("duplicate-second-a", "duplicate-second-b"),
            ),
            ev_completed("duplicate-spawn-parent-response"),
        ]),
    )
    .await;
    let followup = mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            has_function_call_output(request, FIRST_CALL_ID)
                && has_function_call_output(request, SECOND_CALL_ID)
        },
        sse(vec![
            ev_response_created("duplicate-spawn-followup-response"),
            ev_assistant_message("duplicate-spawn-followup-message", "duplicate rejected"),
            ev_completed("duplicate-spawn-followup-response"),
        ]),
    )
    .await;
    let test = spine_builder().build(&server).await?;

    test.submit_turn(PROMPT).await?;

    assert_eq!(
        test.thread_manager.list_thread_ids().await.len(),
        1,
        "duplicate spine.spawn calls must fail before creating children"
    );
    for call_id in [FIRST_CALL_ID, SECOND_CALL_ID] {
        let provider_output = followup
            .function_call_output_text(call_id)
            .with_context(|| format!("missing failure output for `{call_id}`"))?;
        assert_eq!(provider_output, r#"{"status":"failure"}"#);
        let output = persisted_function_call_output(&test, call_id)?;
        assert!(
            output.contains("spine.spawn may be called at most once in one model response"),
            "unexpected durable failure output for `{call_id}`: {output}"
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configured_per_call_bound_is_model_visible_and_rejects_oversized_batches() -> Result<()> {
    const CALL_ID: &str = "spawn-over-limit-call";
    const PROMPT: &str = "run a spine spawn batch beyond the configured per-call bound";
    let tasks = [
        ("first", "first-marker"),
        ("second", "second-marker"),
        ("third", "third-marker"),
    ];
    let server = start_mock_server().await;
    let first_request = mount_sse_once_match(
        &server,
        move |request: &wiremock::Request| {
            body_contains(request, PROMPT)
                && !body_contains(request, BRANCH_PROMPT_MARKER)
                && !has_function_call_output(request, CALL_ID)
        },
        sse(vec![
            ev_response_created("over-limit-parent-response"),
            ev_function_call_with_namespace(
                CALL_ID,
                SPAWN_NAMESPACE,
                SPAWN_TOOL,
                &spawn_args_for(&tasks),
            ),
            ev_completed("over-limit-parent-response"),
        ]),
    )
    .await;
    let parent_followup = mount_sse_once_match(
        &server,
        move |request: &wiremock::Request| {
            has_function_call_output(request, CALL_ID)
                && !body_contains(request, BRANCH_PROMPT_MARKER)
        },
        sse(vec![
            ev_response_created("over-limit-followup-response"),
            ev_assistant_message("over-limit-followup-message", "limit handled"),
            ev_completed("over-limit-followup-response"),
        ]),
    )
    .await;
    let test = spine_builder().build(&server).await?;

    test.submit_turn(PROMPT).await?;

    let request_body = first_request.single_request().body_json();
    let spawn = request_body["tools"]
        .as_array()
        .and_then(|tools| {
            tools
                .iter()
                .find(|tool| tool["type"] == "namespace" && tool["name"] == SPAWN_NAMESPACE)
        })
        .and_then(|namespace| namespace["tools"].as_array())
        .and_then(|tools| tools.iter().find(|tool| tool["name"] == SPAWN_TOOL))
        .context("model request is missing spine.spawn")?;
    assert!(
        spawn["description"]
            .as_str()
            .is_some_and(|description| description.ends_with(
                "The tasks array must contain at least 2 and at most 2 task assignments."
            )),
        "configured task bound must be visible in the tool description"
    );
    assert_eq!(
        spawn["parameters"]["properties"]["tasks"].get("minItems"),
        None
    );
    assert_eq!(
        spawn["parameters"]["properties"]["tasks"].get("maxItems"),
        None
    );
    assert_eq!(
        test.thread_manager.list_thread_ids().await.len(),
        1,
        "per-call validation must run before child creation"
    );
    let provider_output = parent_followup
        .function_call_output_text(CALL_ID)
        .expect("parent follow-up must receive the spine.spawn failure carrier");
    assert_eq!(provider_output, r#"{"status":"failure"}"#);

    let output = persisted_function_call_output(&test, CALL_ID)?;
    assert!(
        output.contains("spine.spawn accepts at most 2 tasks"),
        "unexpected durable failure output: {output}"
    );
    Ok(())
}
