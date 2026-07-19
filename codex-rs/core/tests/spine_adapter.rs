#![allow(clippy::expect_used)]

#[path = "suite/compact_resume_fork.rs"]
mod compact_resume_fork;
#[path = "suite/spine_spawn.rs"]
mod spine_spawn;
#[path = "suite/spine_world_state.rs"]
mod spine_world_state;

use anyhow::Context;
use anyhow::Result;
use codex_features::Feature;
use codex_login::CodexAuth;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::test_codex::spine_test_codex;
use serde_json::Value;
use serde_json::json;
use std::fs;
#[cfg(not(target_os = "windows"))]
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;

#[tokio::test]
async fn spine_adapter_profile_projects_anchored_input_and_status() -> Result<()> {
    let server = start_mock_server().await;
    let response_mock = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("spine-adapter-profile"),
            ev_completed("spine-adapter-profile"),
        ]),
    )
    .await;
    let mut builder = spine_test_codex();
    let test = builder.build(&server).await?;

    assert!(test.config.features.enabled(Feature::SpineJit));
    assert!(!test.config.features.enabled(Feature::SpineTrim));
    assert!(!test.config.features.enabled(Feature::SpineSpawn));

    test.submit_turn("adapter profile probe").await?;

    let input = response_mock.single_request().input();
    let user_text = message_text(&input, "user").context("missing projected user input")?;
    assert_anchored_user_text(user_text, "adapter profile probe")?;
    let status_text = message_text(&input, "developer").context("missing Spine status overlay")?;
    assert!(status_text.starts_with("<spine_status "));
    assert!(status_text.ends_with("/>") || status_text.ends_with(" />"));

    Ok(())
}

#[tokio::test]
async fn spine_adapter_item_ids_cover_projection_only_status() -> Result<()> {
    let server = start_mock_server().await;
    let response_mock = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("spine-adapter-item-ids"),
            ev_completed("spine-adapter-item-ids"),
        ]),
    )
    .await;
    let mut builder = spine_test_codex()
        .with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
        .with_config(|config| {
            config
                .features
                .enable(Feature::ItemIds)
                .expect("ItemIds should be configurable in tests");
        });
    let test = builder.build(&server).await?;

    test.submit_turn("item identity probe").await?;

    let input = response_mock.single_request().input();
    let status = input
        .iter()
        .find(|item| {
            item.get("role").and_then(Value::as_str) == Some("developer")
                && item.to_string().contains("<spine_status ")
        })
        .context("missing projection-only status")?;
    assert!(status.get("id").and_then(Value::as_str).is_some());
    for item in input {
        assert!(
            item.get("id").and_then(Value::as_str).is_some(),
            "model-visible input item is missing an ID: {item:#?}"
        );
    }

    Ok(())
}

#[cfg_attr(windows, ignore = "no exec_command on Windows")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spine_adapter_preserves_host_tool_output_truncation() -> Result<()> {
    let server = start_mock_server().await;
    let response_mock = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("spine-truncation-1"),
                ev_function_call(
                    "large-output",
                    "exec_command",
                    &json!({
                        "cmd": "python3 -c \"import sys; sys.stdout.write('x' * 50000)\""
                    })
                    .to_string(),
                ),
                ev_completed("spine-truncation-1"),
            ]),
            sse(vec![
                ev_response_created("spine-truncation-2"),
                ev_completed("spine-truncation-2"),
            ]),
        ],
    )
    .await;
    let mut builder = spine_test_codex().with_config(|config| {
        config.tool_output_token_limit = Some(50);
    });
    let test = builder.build(&server).await?;

    test.submit_turn("produce a large tool result").await?;

    let requests = response_mock.requests();
    assert_eq!(requests.len(), 2);
    let output = requests[1]
        .function_call_output_text("large-output")
        .context("missing exec_command output")?;
    assert!(
        output.len() < 2_000,
        "Spine projection restored an untruncated tool output: {} bytes",
        output.len()
    );
    assert!(output.contains("tokens truncated"));

    Ok(())
}

#[cfg(not(target_os = "windows"))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spine_adapter_legacy_notify_uses_native_user_evidence() -> Result<()> {
    let server = start_mock_server().await;
    let response_mock = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("spine-notify"),
            ev_completed("spine-notify"),
        ]),
    )
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
    let notify_script_str = notify_script.to_str().context("notify path")?.to_string();
    let mut builder = spine_test_codex().with_config(move |config| {
        config.notify = Some(vec![notify_script_str]);
    });
    let test = builder.build(&server).await?;

    test.submit_turn("native notify probe").await?;
    core_test_support::fs_wait::wait_for_path_exists(
        &notify_file,
        std::time::Duration::from_secs(5),
    )
    .await?;
    let payload: Value = serde_json::from_str(&fs::read_to_string(notify_file)?)?;
    assert_eq!(payload["input-messages"], json!(["native notify probe"]));
    assert_eq!(response_mock.requests().len(), 1);

    Ok(())
}

fn assert_anchored_user_text(actual: &str, expected_body: &str) -> Result<()> {
    let anchored = actual
        .strip_prefix("[U")
        .and_then(|text| text.split_once("]\n"))
        .context("projected user input must have a [U#] anchor")?;
    let (ordinal, body) = anchored;
    anyhow::ensure!(
        !ordinal.is_empty() && ordinal.chars().all(|ch| ch.is_ascii_digit()),
        "projected user anchor ordinal must be numeric"
    );
    anyhow::ensure!(body == expected_body, "projected user body changed");
    Ok(())
}

fn message_text<'a>(input: &'a [Value], role: &str) -> Option<&'a str> {
    input.iter().rev().find_map(|item| {
        (item.get("type").and_then(Value::as_str) == Some("message")
            && item.get("role").and_then(Value::as_str) == Some(role))
        .then(|| {
            item.get("content")
                .and_then(Value::as_array)
                .and_then(|content| content.first())
                .and_then(|content| content.get("text"))
                .and_then(Value::as_str)
        })
        .flatten()
    })
}
