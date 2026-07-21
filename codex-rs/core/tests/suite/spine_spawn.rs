use anyhow::Result;
use codex_features::Feature;
use codex_protocol::AgentPath;
use codex_protocol::protocol::AgentStatus;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::protocol::Op;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ResponseMock;
use core_test_support::responses::ResponsesRequest;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
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
use serde_json::Value;
use serde_json::json;
use std::time::Duration;
use tokio::time::Instant;
use tokio::time::sleep;

const SPAWN_NAMESPACE: &str = "spine";
const SPAWN_TOOL: &str = "spawn";
const SPAWN_CALL_ID: &str = "spawn-lifecycle-call";
const FIRST_PARENT_PROMPT: &str = "run the lifecycle spawn batch";
const SECOND_PARENT_PROMPT: &str = "run the replacement spawn batch";
const CORRECTION_MESSAGE: &str = "你是自依赖的Agent，除了final memory之外不要发送我任何消息。";

fn body_contains(request: &wiremock::Request, text: &str) -> bool {
    decoded_body(request)
        .and_then(|body| serde_json::from_slice::<Value>(&body).ok())
        .is_some_and(|body| body.to_string().contains(text))
}

fn child_task_marker(request: &wiremock::Request, marker: &str) -> bool {
    // The completed parent receipt preserves the exact task prompt as evidence. Require the
    // runtime envelope as well so that parent follow-up requests cannot match a child recorder.
    body_contains(request, marker)
        && body_contains(request, "You are a self-contained spine.spawn child agent")
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
        && !body_contains(request, "You are a self-contained spine.spawn child agent")
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

fn spawn_args(first_marker: &str, second_marker: &str) -> String {
    json!({
        "tasks": [
            {"summary": "first", "prompt": first_marker},
            {"summary": "second", "prompt": second_marker}
        ]
    })
    .to_string()
}

fn spine_builder() -> TestCodexBuilder {
    spine_test_codex()
        .with_spine_spawn()
        .with_model("koffing")
        .with_config(|config| {
            config.multi_agent_v2.max_concurrent_threads_per_session = 3;
            config.model_provider.request_max_retries = Some(0);
            config.model_provider.stream_max_retries = Some(0);
            config.model_provider.supports_websockets = false;
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
                && !request.body_contains_text("You are a self-contained spine.spawn child agent")
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
    assert_eq!(matches.len(), 1, "unique mocked request `{label}`");
    matches.remove(0)
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
                && !body_contains(request, "You are a self-contained spine.spawn child agent")
        },
        sse(vec![
            ev_response_created("parent-followup-response"),
            ev_assistant_message("parent-followup-message", "parent done"),
            ev_completed("parent-followup-response"),
        ]),
    )
    .await;
    let test = spine_builder().build(&server).await?;
    assert!(test.config.features.enabled(Feature::SpineSpawn));
    assert!(!test.config.features.enabled(Feature::MultiAgentV2));
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spawn_starts_batch_concurrently_and_orders_reverse_completion() -> Result<()> {
    let (server, test, parent_spawn, first_child, second_child, parent_followup) =
        build_reverse_completion_fixture(Duration::from_millis(500), Duration::from_millis(100))
            .await?;

    let observe_overlap = async {
        if let Err(error) = wait_for_request(&first_child, "first child", |request| {
            request.body_contains_text("first-child-marker")
                && request.body_contains_text("You are a self-contained spine.spawn child agent")
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
                && request.body_contains_text("You are a self-contained spine.spawn child agent")
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
    assert!(
        rendered.find("first memory") < rendered.find("second memory"),
        "parent projection must preserve task ordinal order"
    );

    let parent_first_request =
        unique_matching_request(&parent_spawn, "initial parent", |request| {
            request.body_contains_text(FIRST_PARENT_PROMPT)
                && !request.body_contains_text("You are a self-contained spine.spawn child agent")
                && request.function_call_output_text(SPAWN_CALL_ID).is_none()
        });
    let child_first_request = unique_matching_request(&first_child, "first child", |request| {
        request.body_contains_text("first-child-marker")
            && request.body_contains_text("You are a self-contained spine.spawn child agent")
    });
    let parent_first_body = parent_first_request.body_json();
    let child_first_body = child_first_request.body_json();
    assert!(!has_namespace(&parent_first_request, "collaboration"));
    assert!(!has_namespace(&child_first_request, "collaboration"));
    assert!(
        parent_first_request
            .tool_by_name("spine", "spawn")
            .is_some()
    );
    assert!(child_first_request.tool_by_name("spine", "spawn").is_some());
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
        !child_input.iter().any(|item| {
            item.get("type").and_then(Value::as_str) == Some("function_call")
                && item.get("name").and_then(Value::as_str) == Some("spawn")
        }),
        "native FullHistory sanitization must not expose the in-flight spawn call"
    );
    let parent_input = parent_first_body["input"]
        .as_array()
        .expect("parent request input must be an array");
    let exact_lcp = parent_input
        .iter()
        .zip(child_input)
        .take_while(|(parent, child)| parent == child)
        .count();
    let parent_cache_key = parent_first_body["prompt_cache_key"]
        .as_str()
        .expect("parent request must expose prompt_cache_key")
        .to_string();
    let child_cache_key = child_first_body["prompt_cache_key"]
        .as_str()
        .expect("child request must expose prompt_cache_key")
        .to_string();
    eprintln!(
        "SPINE_SPAWN_CONTEXT_DIAGNOSTIC {}",
        json!({
            "semantic_parent_prompt": true,
            "parent_input_items": parent_input.len(),
            "child_input_items": child_input.len(),
            "exact_lcp_items": exact_lcp,
            "parent_prompt_cache_key": parent_cache_key,
            "child_prompt_cache_key": child_cache_key,
            "cache_key_equal": parent_cache_key == child_cache_key,
            "native_filtered_in_flight_spawn_call": true,
            "cache_hit_claim": false,
        })
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
                && !body_contains(request, "You are a self-contained spine.spawn child agent")
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
                && request.body_contains_text("You are a self-contained spine.spawn child agent")
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
async fn interrupt_tears_down_children_and_releases_batch_capacity() -> Result<()> {
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
    let cancel_first = mount_response_once_match(
        &server,
        |request: &wiremock::Request| child_task_marker(request, "cancel-first-marker"),
        sse_response(sse(vec![
            ev_response_created("cancel-first-response"),
            ev_assistant_message("cancel-first-message", "too late"),
            ev_completed("cancel-first-response"),
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
        |request: &wiremock::Request| body_contains(request, SECOND_PARENT_PROMPT),
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
    wait_for_request(&cancel_first, "cancel first child", |request| {
        request.body_contains_text("cancel-first-marker")
            && request.body_contains_text("You are a self-contained spine.spawn child agent")
    })
    .await?;
    wait_for_request(&cancel_second, "cancel second child", |request| {
        request.body_contains_text("cancel-second-marker")
            && request.body_contains_text("You are a self-contained spine.spawn child agent")
    })
    .await?;
    test.codex.submit(Op::Interrupt).await?;

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
            request.body_contains_text(marker)
                && request.body_contains_text("You are a self-contained spine.spawn child agent")
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
                && !body_contains(request, "You are a self-contained spine.spawn child agent")
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
                && !body_contains(request, "You are a self-contained spine.spawn child agent")
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
                && !body_contains(request, "You are a self-contained spine.spawn child agent")
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
                && !body_contains(request, "You are a self-contained spine.spawn child agent")
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
