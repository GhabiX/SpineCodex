#![allow(clippy::expect_used)]

#[path = "suite/compact_resume_fork.rs"]
mod compact_resume_fork;
#[path = "suite/spine_spawn.rs"]
mod spine_spawn;

use anyhow::Context;
use anyhow::Result;
use codex_features::Feature;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::test_codex::spine_test_codex;
use serde_json::Value;

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
