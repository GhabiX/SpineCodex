use anyhow::Context;
use anyhow::Result;
use codex_features::Feature;
use core_test_support::responses;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::spine_test_codex;
use pretty_assertions::assert_eq;
use serde_json::Value;

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

fn additional_tools(body: &Value) -> Result<&[Value]> {
    body["input"]
        .as_array()
        .context("Responses request input should be an array")?
        .first()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("additional_tools"))
        .context("Responses request should start with additional_tools")?["tools"]
        .as_array()
        .map(Vec::as_slice)
        .context("additional_tools tools should be an array")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn responses_lite_spine_status_is_the_final_request_input() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let response_mock = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                responses::ev_response_created("resp-status-open"),
                responses::ev_function_call_with_namespace(
                    "status-open",
                    "spine",
                    "open",
                    r#"{"summary":"status child"}"#,
                ),
                responses::ev_completed("resp-status-open"),
            ]),
            responses::sse(vec![
                responses::ev_response_created("resp-status-done"),
                responses::ev_completed("resp-status-done"),
            ]),
        ],
    )
    .await;
    let mut builder = spine_test_codex().with_model_info_override("gpt-5.4", |model_info| {
        model_info.use_responses_lite = true;
    });
    let test = builder.build(&server).await?;

    test.submit_turn("status tail").await?;

    let requests = response_mock.requests();
    assert_eq!(requests.len(), 2);
    for (request, cursor) in requests.iter().zip(["1", "1.1"]) {
        let input = request.input();
        let last = input.last().context("request input should not be empty")?;
        assert_eq!(last["type"], "message");
        assert_eq!(last["role"], "developer");
        let text = last["content"][0]["text"]
            .as_str()
            .context("status input should contain text")?;
        assert!(text.starts_with("<spine_status "), "{text}");
        assert!(text.contains(&format!(r#"cursor="{cursor}""#)), "{text}");
        for field in [
            "cursor=",
            "summary=",
            "parent=",
            "parent_summary=",
            "cursor_context=",
            "context_left=",
        ] {
            assert!(text.contains(field), "missing {field} in {text}");
        }
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn responses_lite_spine_memory_slots_precede_the_status_overlay() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let response_mock = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                responses::ev_response_created("resp-memory-open"),
                responses::ev_function_call_with_namespace(
                    "memory-open",
                    "spine",
                    "open",
                    r#"{"summary":"memory child"}"#,
                ),
                responses::ev_completed("resp-memory-open"),
            ]),
            responses::sse(vec![
                responses::ev_response_created("resp-memory-opened"),
                responses::ev_completed("resp-memory-opened"),
            ]),
            responses::sse(vec![
                responses::ev_response_created("resp-memory-close"),
                responses::ev_function_call_with_namespace(
                    "memory-close",
                    "spine",
                    "close",
                    r#"{"memory":"child complete"}"#,
                ),
                responses::ev_completed("resp-memory-close"),
            ]),
            responses::sse(vec![
                responses::ev_response_created("resp-memory-done"),
                responses::ev_completed("resp-memory-done"),
            ]),
        ],
    )
    .await;
    let mut builder = spine_test_codex().with_model_info_override("gpt-5.4", |model_info| {
        model_info.use_responses_lite = true;
    });
    let test = builder.build(&server).await?;

    test.submit_turn("root request").await?;
    test.submit_turn("child request").await?;

    let requests = response_mock.requests();
    assert_eq!(requests.len(), 4);
    let input = requests[3].input();
    let user_texts = input
        .iter()
        .enumerate()
        .filter(|(_, item)| item.get("role").and_then(Value::as_str) == Some("user"))
        .filter_map(|(index, item)| {
            item["content"][0]["text"]
                .as_str()
                .map(|text| (index, text))
        })
        .collect::<Vec<_>>();
    let child_user = user_texts
        .iter()
        .find(|(_, text)| text.starts_with("[U") && text.ends_with("\nchild request"))
        .with_context(|| format!("closed child user slot should be present: {user_texts:#?}"))?;
    let child_summary = user_texts
        .iter()
        .find(|(_, text)| {
            *text == "<spine_memory node_id=\"1.1\">\nchild complete\n</spine_memory>"
        })
        .context("closed child summary slot should be present")?;
    let status_index = input.len() - 1;
    assert!(child_user.0 < child_summary.0);
    assert!(child_summary.0 < status_index);
    let status = &input[status_index];
    assert_eq!(status["role"], "developer");
    let status_text = status["content"][0]["text"]
        .as_str()
        .context("status input should contain text")?;
    assert!(status_text.starts_with("<spine_status "), "{status_text}");
    for field in [
        "cursor=",
        "summary=",
        "parent=",
        "parent_summary=",
        "cursor_context=",
        "context_left=",
    ] {
        assert!(
            status_text.contains(field),
            "missing {field} in {status_text}"
        );
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn responses_lite_exposes_spine_tools_as_a_native_namespace() -> Result<()> {
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
    let mut builder = spine_test_codex()
        .with_model_info_override("gpt-5.4", |model_info| {
            model_info.use_responses_lite = true;
        })
        .with_config(|config| {
            config
                .features
                .enable(Feature::CodeMode)
                .expect("enable CodeMode");
        });
    let test = builder.build(&server).await?;

    test.submit_turn("inspect Spine tools").await?;

    let body = response_mock.single_request().body_json();
    let tools = additional_tools(&body)?;
    for tool_name in ["open", "close", "next"] {
        assert!(
            has_namespaced_tool(tools, "spine", tool_name),
            "missing spine.{tool_name} native namespace tool"
        );
    }
    let exec_description = tools
        .iter()
        .find(|tool| tool.get("type").and_then(Value::as_str) == Some("custom"))
        .and_then(|tool| tool.get("description"))
        .and_then(Value::as_str)
        .context("Responses Lite request should contain the exec tool")?;
    assert!(!exec_description.contains("spine__open"));
    assert!(!exec_description.contains("spine__close"));
    assert!(!exec_description.contains("spine__next"));

    Ok(())
}
