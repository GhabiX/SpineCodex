#![allow(clippy::expect_used)]

use std::fs;
#[cfg(not(target_os = "windows"))]
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::RolloutLine;
use codex_protocol::user_input::UserInput;
use core_test_support::responses;
use core_test_support::streaming_sse::StreamingSseChunk;
use core_test_support::streaming_sse::start_streaming_sse_server;
use core_test_support::test_codex::TestCodex;
use core_test_support::test_codex::spine_test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use spine_core::SamplingArchiveRecord;
use spine_core::SpineOperationFact;
#[cfg(not(target_os = "windows"))]
use tempfile::TempDir;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retry_after_spine_effect_commits_failed_attempt_and_projects_once() -> Result<()> {
    let failed_after_open = responses::sse(vec![
        responses::ev_response_created("failed-after-open"),
        responses::ev_function_call_with_namespace(
            "failed-after-open-call",
            "spine",
            "open",
            r#"{"summary":"durable failed-stream child"}"#,
        ),
    ]);
    let completed = responses::sse(vec![
        responses::ev_response_created("failed-after-open-retry"),
        responses::ev_assistant_message("failed-after-open-message", "retry complete"),
        responses::ev_completed("failed-after-open-retry"),
    ]);
    let (server, _) = start_streaming_sse_server(vec![
        vec![StreamingSseChunk {
            gate: None,
            body: failed_after_open,
        }],
        vec![StreamingSseChunk {
            gate: None,
            body: completed,
        }],
    ])
    .await;
    let mut builder = spine_test_codex().with_config(|config| {
        config.model_provider.request_max_retries = Some(0);
        config.model_provider.stream_max_retries = Some(1);
        config.model_provider.supports_websockets = false;
    });
    let test = builder.build_with_streaming_server(&server).await?;

    test.submit_turn("preserve a Spine effect across retry")
        .await?;
    test.codex.flush_rollout().await?;

    let requests = server.requests().await;
    let request_bodies = requests
        .iter()
        .map(|body| serde_json::from_slice::<Value>(body))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let records = load_sampling_records(&test)?;
    let commits = sampling_commits(&records);
    assert_eq!(request_bodies.len(), 2, "sampling records: {records:#?}");
    assert_eq!(commits.len(), 2, "sampling records: {records:#?}");
    assert_eq!(commits[0].executions.len(), 1);
    assert!(matches!(
        commits[0].executions[0].operation,
        SpineOperationFact::Open { ref summary }
            if summary == "durable failed-stream child"
    ));
    assert!(commits[1].executions.is_empty());
    assert_eq!(
        input_text_occurrences(&request_bodies[1], "<spine_node"),
        1,
        "retry request must contain exactly one installed Spine node projection"
    );
    assert_eq!(
        input_text_occurrences(&request_bodies[1], "durable failed-stream child"),
        1,
        "retry request must not duplicate the installed failed-attempt transition"
    );

    server.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupt_after_spine_effect_commits_cancelled_attempt_once() -> Result<()> {
    let server = responses::start_mock_server().await;
    #[cfg(not(target_os = "windows"))]
    let command = "sleep 60";
    #[cfg(target_os = "windows")]
    let command = "Start-Sleep -Seconds 60";
    let response_mock = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                responses::ev_response_created("cancelled-after-open"),
                responses::ev_function_call_with_namespace(
                    "cancelled-open-call",
                    "spine",
                    "open",
                    r#"{"summary":"durable cancelled child"}"#,
                ),
                responses::ev_function_call(
                    "cancelled-blocking-call",
                    "shell_command",
                    &json!({
                        "command": command,
                        "timeout_ms": 60_000,
                    })
                    .to_string(),
                ),
                responses::ev_completed("cancelled-after-open"),
            ]),
            responses::sse(vec![
                responses::ev_response_created("after-cancel-projection"),
                responses::ev_assistant_message("after-cancel-message", "continued"),
                responses::ev_completed("after-cancel-projection"),
            ]),
        ],
    )
    .await;
    let test = spine_test_codex().build_with_auto_env(&server).await?;

    test.codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "commit the Spine effect before interrupting".to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: Default::default(),
        })
        .await?;
    wait_for_event(&test.codex, |event| {
        matches!(
            event,
            EventMsg::RawResponseItem(raw)
                if matches!(
                    &raw.item,
                    codex_protocol::models::ResponseItem::FunctionCallOutput { call_id, .. }
                        if call_id == "cancelled-open-call"
                )
        )
    })
    .await;
    test.codex.submit(Op::Interrupt).await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnAborted(_))
    })
    .await;
    test.submit_turn("continue after the interrupted effect")
        .await?;
    test.codex.flush_rollout().await?;

    let records = load_sampling_records(&test)?;
    let commits = sampling_commits(&records);
    let requests = response_mock.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        input_text_occurrences(&requests[1].body_json(), "<spine_node"),
        1,
        "the first post-interrupt request must project the cancelled effect exactly once"
    );
    assert_eq!(
        input_text_occurrences(&requests[1].body_json(), "durable cancelled child"),
        1,
        "the cancelled effect must not be duplicated after interruption"
    );
    assert_eq!(commits.len(), 2, "sampling records: {records:#?}");
    assert_eq!(commits[0].executions.len(), 1);
    assert!(matches!(
        commits[0].executions[0].operation,
        SpineOperationFact::Open { ref summary } if summary == "durable cancelled child"
    ));
    assert!(commits[1].executions.is_empty());

    Ok(())
}

#[cfg(not(target_os = "windows"))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retry_after_close_uses_first_sampling_input_for_legacy_notify() -> Result<()> {
    let opened = responses::sse(vec![
        responses::ev_response_created("spine-notify-open"),
        responses::ev_function_call_with_namespace(
            "spine-notify-open-call",
            "spine",
            "open",
            r#"{"summary":"notify retry child"}"#,
        ),
        responses::ev_completed("spine-notify-open"),
    ]);
    let opened_done = responses::sse(vec![
        responses::ev_assistant_message("spine-notify-open-done", "child ready"),
        responses::ev_completed("spine-notify-open-done"),
    ]);
    let close_incomplete = responses::sse(vec![
        responses::ev_response_created("spine-notify-close-incomplete"),
        responses::ev_function_call_with_namespace(
            "spine-notify-close-call",
            "spine",
            "close",
            r#"{"memory":"closed child memory"}"#,
        ),
    ]);
    let retry_done = responses::sse(vec![
        responses::ev_assistant_message("spine-notify-retry-done", "done"),
        responses::ev_completed("spine-notify-retry-done"),
    ]);
    let (server, _) = start_streaming_sse_server(vec![
        vec![StreamingSseChunk {
            gate: None,
            body: opened,
        }],
        vec![StreamingSseChunk {
            gate: None,
            body: opened_done,
        }],
        vec![StreamingSseChunk {
            gate: None,
            body: close_incomplete,
        }],
        vec![StreamingSseChunk {
            gate: None,
            body: retry_done,
        }],
    ])
    .await;
    let notify_dir = TempDir::new()?;
    let notify_script = notify_dir.path().join("notify.sh");
    fs::write(
        &notify_script,
        r#"#!/bin/bash
set -e
payload_path="$(dirname "${0}")/notify.jsonl"
printf '%s\n' "${@: -1}" >> "${payload_path}""#,
    )?;
    fs::set_permissions(&notify_script, fs::Permissions::from_mode(0o755))?;
    let notify_file = notify_dir.path().join("notify.jsonl");
    let notify_script_str = notify_script
        .to_str()
        .context("notify path must be UTF-8")?
        .to_string();
    let mut builder = spine_test_codex().with_config(move |config| {
        config.notify = Some(vec![notify_script_str]);
        config.model_provider.request_max_retries = Some(0);
        config.model_provider.stream_max_retries = Some(1);
        config.model_provider.supports_websockets = false;
    });
    let test = builder.build_with_streaming_server(&server).await?;

    test.submit_turn("open a child for notify retry").await?;
    test.submit_turn("child evidence removed by close").await?;

    let notify_payloads = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(contents) = fs::read_to_string(&notify_file) {
                let payloads = contents
                    .lines()
                    .map(serde_json::from_str::<Value>)
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                if payloads.len() >= 2 {
                    return Ok::<_, serde_json::Error>(payloads);
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .context("timed out waiting for legacy notify payloads")??;
    test.codex.flush_rollout().await?;

    let requests = server.requests().await;
    let request_bodies = requests
        .iter()
        .map(|body| serde_json::from_slice::<Value>(body))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(request_bodies.len(), 4);
    let failed_input = &request_bodies[2];
    let retry_input = &request_bodies[3];
    assert_eq!(
        input_text_occurrences(failed_input, "closed child memory"),
        0
    );
    assert_eq!(
        input_text_occurrences(retry_input, "closed child memory"),
        1
    );
    assert_eq!(input_text_occurrences(retry_input, "<spine_memory"), 1);
    assert_eq!(
        notify_payloads
            .last()
            .context("missing final legacy notify payload")?["input-messages"],
        json!(user_messages(failed_input))
    );

    let records = load_sampling_records(&test)?;
    let commits = sampling_commits(&records);
    let close_commits = commits
        .iter()
        .filter(|commit| {
            commit.executions.iter().any(|execution| {
                matches!(
                    execution.operation,
                    SpineOperationFact::Close { ref memory } if memory == "closed child memory"
                )
            })
        })
        .count();
    assert_eq!(close_commits, 1, "sampling records: {records:#?}");

    server.shutdown().await;
    Ok(())
}

fn input_text_occurrences(request: &Value, needle: &str) -> usize {
    request["input"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item.get("content").and_then(Value::as_array))
        .flatten()
        .filter_map(|content| content.get("text").and_then(Value::as_str))
        .map(|text| text.matches(needle).count())
        .sum()
}

fn user_messages(request: &Value) -> Vec<String> {
    request["input"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| serde_json::from_value(item.clone()).ok())
        .filter_map(|item| match codex_core::parse_turn_item(&item) {
            Some(codex_protocol::items::TurnItem::UserMessage(message)) => Some(message.message()),
            _ => None,
        })
        .collect()
}

fn sampling_commits(records: &[SamplingArchiveRecord]) -> Vec<&spine_core::SamplingCommit> {
    records
        .iter()
        .filter_map(|record| match record {
            SamplingArchiveRecord::SamplingStarted(_) => None,
            SamplingArchiveRecord::SamplingCommit(commit) => Some(commit),
        })
        .collect()
}

fn load_sampling_records(test: &TestCodex) -> Result<Vec<SamplingArchiveRecord>> {
    let rollout = fs::read_to_string(test.codex.rollout_path().context("rollout path")?)?;
    rollout
        .lines()
        .map(serde_json::from_str::<RolloutLine>)
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .filter_map(|line| match line.item {
            RolloutItem::SpineSamplingStarted(item) => Some(item.payload),
            RolloutItem::SpineTransition(item) => Some(item.payload),
            _ => None,
        })
        .map(|payload| {
            SamplingArchiveRecord::decode(&serde_json::to_vec(&payload)?)
                .map_err(anyhow::Error::new)
        })
        .collect()
}
