use std::collections::HashMap;

use anyhow::Result;
use codex_core::TurnInputRequest;
use codex_features::Feature;
use codex_protocol::items::AgentMessageDelivery;
use codex_protocol::items::TurnItem;
use codex_protocol::openai_models::ToolMode;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::request_user_input::RequestUserInputAnswer;
use codex_protocol::request_user_input::RequestUserInputResponse;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call_with_namespace;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::responses::{self};
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::spine_test_codex;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use core_test_support::wait_for_event_match;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;

fn native_tools(body: &Value) -> Vec<Value> {
    body["tools"]
        .as_array()
        .expect("tool catalog")
        .iter()
        .filter(|tool| tool["name"] != spine_core::host::SPINE_NAMESPACE)
        .cloned()
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spine_user_input_sync_preserves_native_catalog_and_answer() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let mut catalogs = Vec::new();
    for (index, builder) in [test_codex(), spine_test_codex()].into_iter().enumerate() {
        let server = start_mock_server().await;
        let questions = json!([{
            "id": "confirm", "header": "Confirm", "question": "Continue?",
            "options": [{"label": "Yes", "description": "Continue"}, {"label": "No", "description": "Stop"}]
        }]);
        let requests = responses::mount_sse_sequence(
            &server,
            vec![
                sse(vec![
                    ev_response_created("first"),
                    ev_function_call_with_namespace(
                        "confirm-call",
                        "functions",
                        "request_user_input",
                        &json!({"questions": questions}).to_string(),
                    ),
                    ev_completed("first"),
                ]),
                sse(vec![
                    ev_response_created("second"),
                    ev_assistant_message("done", "Confirmed."),
                    ev_completed("second"),
                ]),
            ],
        )
        .await;
        let test = builder
            .with_config(|config| {
                config
                    .features
                    .enable(Feature::DefaultModeRequestUserInput)
                    .expect("enable sync question");
            })
            .build_with_auto_env(&server)
            .await?;
        test.codex
            .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
                text: "Ask me before continuing.".to_string(),
                text_elements: Vec::new(),
            }]))
            .await?;
        let question = wait_for_event_match(test.codex.as_ref(), |event| {
            if let EventMsg::RequestUserInput(question) = event {
                Some(question.clone())
            } else {
                None
            }
        })
        .await;
        assert_eq!(question.call_id, "confirm-call");
        assert_eq!(question.questions.len(), 1);
        assert_eq!(question.questions[0].question, "Continue?");
        let mut answers = HashMap::new();
        answers.insert(
            "confirm".to_string(),
            RequestUserInputAnswer {
                answers: vec!["Yes".to_string()],
            },
        );
        test.codex
            .submit(Op::UserInputAnswer {
                id: question.turn_id,
                response: RequestUserInputResponse { answers },
            })
            .await?;
        wait_for_event(test.codex.as_ref(), |event| {
            matches!(event, EventMsg::TurnComplete(_))
        })
        .await;
        let captured = requests.requests();
        assert_eq!(captured.len(), 2);
        assert_eq!(
            serde_json::from_str::<Value>(
                &captured[1]
                    .function_call_output_text("confirm-call")
                    .expect("answer output")
            )?,
            json!({"answers":{"confirm":{"answers":["Yes"]}}})
        );
        let question_count = captured[1]
            .input()
            .into_iter()
            .filter(|item| item["type"] == "function_call" && item["call_id"] == "confirm-call")
            .count();
        let answer_count = captured[1]
            .input()
            .into_iter()
            .filter(|item| {
                item["type"] == "function_call_output" && item["call_id"] == "confirm-call"
            })
            .count();
        assert_eq!(question_count, 1);
        assert_eq!(answer_count, 1);
        let body = captured[0].body_json();
        assert_eq!(
            body["tools"]
                .as_array()
                .expect("tools")
                .iter()
                .filter(|tool| tool["name"] == spine_core::host::SPINE_NAMESPACE)
                .count(),
            index
        );
        catalogs.push(native_tools(&body));
        test.codex.shutdown_and_wait().await?;
    }
    assert_eq!(catalogs[1], catalogs[0]);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spine_user_input_async_preserves_questions_catalog_and_ordinary_reply() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let mut catalogs = Vec::new();
    for (index, builder) in [test_codex(), spine_test_codex()].into_iter().enumerate() {
        let server = start_mock_server().await;
        let requests = responses::mount_sse_sequence(&server, vec![
            sse(vec![ev_response_created("first"), ev_function_call_with_namespace("question-call", "functions", "request_user_input_async", &json!({"questions": [{"title": "Environment?", "options": ["Staging", "Production"]}]}).to_string()), ev_completed("first")]),
            sse(vec![ev_response_created("second"), ev_assistant_message("done", "Waiting for your answer."), ev_completed("second")]),
            sse(vec![ev_response_created("third"), ev_assistant_message("reply", "Answer received."), ev_completed("third")]),
        ]).await;
        let test = builder
            .with_model_info_override("gpt-5.2", |model| {
                model.tool_mode = Some(ToolMode::CodeModeOnly);
                model.experimental_supported_tools = vec!["request_user_input_async".to_string()];
            })
            .build_with_auto_env(&server)
            .await?;
        test.codex
            .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
                text: "Ask which environment.".to_string(),
                text_elements: Vec::new(),
            }]))
            .await?;
        let mut started = Vec::new();
        let mut completed = Vec::new();
        loop {
            let event = test.codex.next_event().await?;
            match event.msg {
                EventMsg::ItemStarted(event) => {
                    if let TurnItem::AgentMessage(message) = event.item
                        && message.delivery == Some(AgentMessageDelivery::Async)
                    {
                        started.push(serde_json::to_value(message)?);
                    }
                }
                EventMsg::ItemCompleted(event) => {
                    if let TurnItem::AgentMessage(message) = event.item
                        && message.delivery == Some(AgentMessageDelivery::Async)
                    {
                        completed.push(serde_json::to_value(message)?);
                    }
                }
                EventMsg::TurnComplete(_) => break,
                _ => {}
            }
        }
        assert_eq!(started.len(), 1);
        assert_eq!(completed, started);
        assert_eq!(
            started[0]["questions"],
            json!([{"title":"Environment?", "options":["Staging", "Production"]}])
        );
        test.submit_turn("ASYNC_REPLY_STAGING").await?;
        let captured = requests.requests();
        assert_eq!(captured.len(), 3);
        assert_eq!(
            captured[1].function_call_output_text("question-call"),
            Some(r#"{"accepted":true}"#.to_string())
        );
        assert_eq!(
            captured[2]
                .message_input_texts("user")
                .iter()
                .map(|text| text.matches("ASYNC_REPLY_STAGING").count())
                .sum::<usize>(),
            1
        );
        assert_eq!(
            captured[2]
                .message_input_texts("assistant")
                .iter()
                .map(|text| text.matches("Environment?").count())
                .sum::<usize>(),
            0
        );
        let body = captured[0].body_json();
        assert_eq!(
            body["tools"]
                .as_array()
                .expect("tools")
                .iter()
                .filter(|tool| tool["name"] == spine_core::host::SPINE_NAMESPACE)
                .count(),
            index
        );
        catalogs.push(native_tools(&body));
        test.codex.shutdown_and_wait().await?;
    }
    assert_eq!(catalogs[1], catalogs[0]);
    Ok(())
}
