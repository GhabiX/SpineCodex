use anyhow::Context;
use anyhow::Result;
use codex_core::compact::SUMMARIZATION_PROMPT;
use codex_features::Feature;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::request_user_input::RequestUserInputAnswer;
use codex_protocol::request_user_input::RequestUserInputResponse;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_completed_with_tokens;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::test_codex::spine_test_codex;
use core_test_support::wait_for_event;
use core_test_support::wait_for_event_match;
use futures::SinkExt;
use futures::StreamExt;
use serde_json::Value;
use serde_json::json;
use std::collections::HashMap;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;

async fn read_exec_server_json(websocket: &mut WebSocketStream<TcpStream>) -> Value {
    loop {
        match timeout(Duration::from_secs(5), websocket.next())
            .await
            .expect("websocket read should not time out")
            .expect("websocket should stay open")
            .expect("websocket frame should read")
        {
            Message::Text(text) => {
                return serde_json::from_str(text.as_ref()).expect("valid JSON-RPC message");
            }
            Message::Binary(bytes) => {
                return serde_json::from_slice(bytes.as_ref()).expect("valid JSON-RPC message");
            }
            Message::Ping(_) | Message::Pong(_) => {}
            other => panic!("expected JSON-RPC message, got {other:?}"),
        }
    }
}

async fn serve_environment_info(listener: TcpListener) {
    let (stream, _) = listener.accept().await.expect("connection");
    let mut websocket = accept_async(stream).await.expect("websocket handshake");
    let initialize = read_exec_server_json(&mut websocket).await;
    assert_eq!(initialize["method"], "initialize");
    websocket
        .send(Message::Text(
            json!({
                "id": initialize["id"],
                "result": { "sessionId": "test-session" }
            })
            .to_string()
            .into(),
        ))
        .await
        .expect("initialize response");
    assert_eq!(
        read_exec_server_json(&mut websocket).await["method"],
        "initialized"
    );
    let info = read_exec_server_json(&mut websocket).await;
    assert_eq!(info["method"], "environment/info");
    websocket
        .send(Message::Text(
            json!({
                "id": info["id"],
                "result": { "shell": { "name": "zsh", "path": "/bin/zsh" } }
            })
            .to_string()
            .into(),
        ))
        .await
        .expect("environment info response");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spine_adapter_compaction_preserves_then_updates_environment_once() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let server = start_mock_server().await;
    let response_mock = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-1"),
                ev_function_call(
                    "wait-for-startup",
                    "request_user_input",
                    &json!({
                        "questions": [{
                            "id": "continue",
                            "header": "Continue",
                            "question": "Continue after startup?",
                            "options": [{
                                "label": "Yes (Recommended)",
                                "description": "Continue the test."
                            }, {
                                "label": "No",
                                "description": "Stop the test."
                            }]
                        }]
                    })
                    .to_string(),
                ),
                ev_completed_with_tokens("resp-1", 96),
            ]),
            sse(vec![
                ev_assistant_message("msg-compact", "AUTO_COMPACT_SUMMARY"),
                ev_completed_with_tokens("resp-compact", 10),
            ]),
            sse(vec![
                ev_response_created("resp-2"),
                ev_assistant_message("msg-2", "done"),
                ev_completed("resp-2"),
            ]),
        ],
    )
    .await;
    let mut builder = spine_test_codex()
        .with_exec_server_url(format!("ws://{}", listener.local_addr()?))
        .with_config(|config| {
            config.project_doc_max_bytes = 0;
            config
                .features
                .enable(Feature::DeferredExecutor)
                .expect("DeferredExecutor should be configurable");
            config
                .features
                .enable(Feature::DefaultModeRequestUserInput)
                .expect("request_user_input should be configurable");
            config.model_provider.name = "OpenAI (test)".to_string();
            config.compact_prompt = Some(SUMMARIZATION_PROMPT.to_string());
            config.model_context_window = Some(100);
            config.model_auto_compact_token_limit = Some(90);
        });
    let test = timeout(Duration::from_secs(5), builder.build(&server))
        .await
        .context("thread startup should not wait for the remote environment")??;

    test.codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "wait for the environment".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: Default::default(),
        })
        .await?;
    let request = wait_for_event_match(&test.codex, |event| match event {
        EventMsg::RequestUserInput(request) => Some(request.clone()),
        _ => None,
    })
    .await;

    serve_environment_info(listener).await;
    test.codex
        .submit(Op::UserInputAnswer {
            id: request.turn_id,
            response: RequestUserInputResponse {
                answers: HashMap::from([(
                    "continue".to_string(),
                    RequestUserInputAnswer {
                        answers: vec!["Yes (Recommended)".to_string()],
                    },
                )]),
            },
        })
        .await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    let requests = response_mock.requests();
    assert_eq!(requests.len(), 3);
    let post_compaction_context = requests[2].message_input_texts("user");
    assert_eq!(
        post_compaction_context
            .iter()
            .filter(|text| text.contains("<status>starting</status>"))
            .count(),
        1
    );
    assert_eq!(
        post_compaction_context
            .iter()
            .filter(|text| text.contains("<shell>zsh</shell>"))
            .count(),
        1
    );
    let starting_index = post_compaction_context
        .iter()
        .position(|text| text.contains("<status>starting</status>"))
        .context("compaction should preserve the old environment state")?;
    let ready_index = post_compaction_context
        .iter()
        .position(|text| text.contains("<shell>zsh</shell>"))
        .context("the next step should report the ready environment")?;
    assert!(starting_index < ready_index);

    Ok(())
}
