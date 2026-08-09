use anyhow::Context;
use anyhow::Result;
use codex_features::Feature;
use codex_protocol::AgentPath;
use codex_protocol::protocol::AgentStatus;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::protocol::MULTI_AGENT_MODE_OPEN_TAG;
use codex_protocol::protocol::MultiAgentVersion;
use codex_protocol::protocol::Op;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ResponseMock;
use core_test_support::responses::ResponsesRequest;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call_with_namespace;
use core_test_support::responses::ev_reasoning_item;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_response_once_match;
use core_test_support::responses::mount_sse_once_match;
use core_test_support::responses::sse;
use core_test_support::responses::sse_response;
use core_test_support::responses::start_mock_server;
use core_test_support::test_codex::TestCodex;
use core_test_support::test_codex::TestCodexBuilder;
use core_test_support::test_codex::spine_test_codex;
use serde_json::Value;
use serde_json::json;
use std::time::Duration;
use tokio::time::Instant;
use tokio::time::sleep;

const SPAWN_NAMESPACE: &str = "spine";
const SPAWN_TOOL: &str = "spawn";
const SPAWN_CALL_ID: &str = "spawn-lifecycle-call";
const SEED_PARENT_PROMPT: &str = "seed reasoning context before spawn";
const FIRST_PARENT_PROMPT: &str = "run the lifecycle spawn batch";
const SECOND_PARENT_PROMPT: &str = "run the replacement spawn batch";
const BRANCH_PROMPT_MARKER: &str = "You are a spawned execution branch.";

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
        .with_model("gpt-5.6-sol")
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

async fn wait_for_request(
    mock_response: &ResponseMock,
    label: &str,
    predicate: impl Fn(&core_test_support::responses::ResponsesRequest) -> bool,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
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
    let _seed_response = mount_sse_once_match(
        &server,
        |request: &wiremock::Request| body_contains(request, SEED_PARENT_PROMPT),
        sse(vec![
            ev_response_created("seed-response"),
            ev_reasoning_item("seed-reasoning-without-content", &["omitted"], &[]),
            ev_reasoning_item(
                "seed-reasoning-with-content",
                &["present"],
                &["raw reasoning content"],
            ),
            ev_assistant_message("seed-message", "seed complete"),
            ev_completed("seed-response"),
        ]),
    )
    .await;
    let parent_spawn = mount_sse_once_match(
        &server,
        is_parent_spawn_request,
        sse(vec![
            ev_response_created("parent-spawn-response"),
            ev_reasoning_item("parent-spawn-reasoning", &["plan spawn batch"], &[]),
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
        .expect("selected model metadata should be present in the test model catalog");
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
fn spine_spawn_respects_metadata_v2_when_multi_agent_feature_is_off() -> Result<()> {
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
    test.submit_turn(SEED_PARENT_PROMPT).await?;
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
    assert!(parent_first_request.body_contains_text("metadata-v2-root-usage-hint"));
    assert!(!parent_first_request.body_contains_text("metadata-v2-subagent-usage-hint"));
    assert!(child_first_request.body_contains_text("metadata-v2-root-usage-hint"));
    assert!(child_first_request.body_contains_text("metadata-v2-subagent-usage-hint"));
    for request in [&parent_first_request, &child_first_request] {
        assert!(request.body_contains_text(MULTI_AGENT_MODE_OPEN_TAG));
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
    assert!(
        child_input.len() >= parent_input.len() + 2,
        "child must append the V2 subagent identity and task envelope after the inherited parent input"
    );
    assert!(
        parent_input.iter().any(|item| {
            item.get("type").and_then(Value::as_str) == Some("reasoning")
                && item.get("content").is_none()
        }),
        "parent request should contain a reasoning item with omitted content"
    );
    assert!(
        parent_input.iter().any(|item| {
            item.get("type").and_then(Value::as_str) == Some("reasoning")
                && item.get("content").is_some()
        }),
        "parent request should contain a reasoning item with serialized content"
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

