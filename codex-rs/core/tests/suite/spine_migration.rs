//! Cold replay of historical Spine archives and current native provider metadata.
use anyhow::Context;
use anyhow::Result;
use codex_features::Feature;
use core_test_support::responses;
use core_test_support::test_codex::spine_test_codex;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_metadata_survives_spine_open_persistence_and_cold_resume() -> Result<()> {
    let server = responses::start_mock_server().await;
    let mut commentary =
        responses::ev_assistant_message("metadata-message", "metadata-before-open");
    commentary["item"]["phase"] = json!("commentary");
    commentary["item"]["status"] = json!("completed");
    commentary["item"]["encrypted_content"] = json!("encrypted-message");
    commentary["item"]["content"][0]["annotations"] = json!([{
        "type": "url_citation", "url": "https://example.test/source",
        "title": "Source", "start_index": 0, "end_index": 1
    }]);
    commentary["item"]["content"][0]["logprobs"] = json!([{
        "token": "metadata", "logprob": -0.25, "bytes": [109], "top_logprobs": []
    }]);
    let mut call = responses::ev_function_call_with_namespace(
        "metadata-open",
        "spine",
        "open",
        r#"{"summary":"metadata scope"}"#,
    );
    call["item"]["status"] = json!("completed");
    call["item"]["encrypted_content"] = json!("encrypted-open");
    let expected =
        responses::strip_response_item_ids_from_json(json!([commentary["item"], call["item"]]));
    let mock = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                responses::ev_response_created("metadata-first"),
                commentary,
                call,
                responses::ev_completed("metadata-first"),
            ]),
            responses::sse(vec![
                responses::ev_assistant_message("metadata-answer", "Opened."),
                responses::ev_completed("metadata-answer"),
            ]),
            responses::sse(vec![
                responses::ev_assistant_message("metadata-resumed", "Continued."),
                responses::ev_completed("metadata-resumed"),
            ]),
        ],
    )
    .await;
    let mut builder = spine_test_codex()
        .with_model("gpt-5.4")
        .with_model_info_override("gpt-5.4", |model| model.use_responses_lite = true)
        .with_config(|config| {
            config
                .features
                .enable(Feature::OutputTokenReplay)
                .expect("enable token replay");
        });
    let test = builder.build_with_auto_env(&server).await?;
    test.submit_turn("Open a scope without dropping sampled metadata.")
        .await?;
    let rollout_path = test.codex.rollout_path().context("rollout path")?;
    let resumed = builder.restart_with_auto_env(&server, &test).await?;
    resumed
        .submit_turn("Continue the scope after cold resume.")
        .await?;

    let selected = |items: &Value| -> Value {
        json!(
            items
                .as_array()
                .expect("input items")
                .iter()
                .filter(|item| {
                    item["content"][0]["text"] == "metadata-before-open"
                        || item["call_id"] == "metadata-open" && item["type"] == "function_call"
                })
                .cloned()
                .collect::<Vec<_>>()
        )
    };
    let requests = mock.requests();
    assert_eq!(requests.len(), 3);
    for request in [&requests[1], &requests[2]] {
        let body = request.body_json();
        assert_eq!(
            responses::strip_metadata_from_json(responses::strip_response_item_ids_from_json(
                selected(&body["input"])
            )),
            expected,
        );
        assert!(request.body_contains_text("<spine_node"));
    }
    let records = std::fs::read_to_string(rollout_path)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<serde_json::Result<Vec<_>>>()?;
    let stored = json!(
        records
            .iter()
            .filter(|record| record["type"] == "response_item")
            .map(|record| record["payload"].clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        responses::strip_metadata_from_json(responses::strip_response_item_ids_from_json(
            selected(&stored)
        )),
        expected,
    );
    resumed.codex.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cold_resume_v033_sampling_archive_preserves_branch_and_continues() -> Result<()> {
    let server = responses::start_mock_server().await;
    let response = responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_response_created("legacy-resumed"),
            responses::ev_assistant_message("legacy-answer", "Continued the historical branch."),
            responses::ev_completed("legacy-resumed"),
        ]),
    )
    .await;
    let home = Arc::new(tempfile::tempdir()?);
    let rollout = home.path().join("historical-rollout.jsonl");
    let fixture = std::fs::read_to_string(codex_utils_cargo_bin::find_resource!(
        "tests/fixtures/spine/rollout-v033.jsonl"
    )?)?;
    // Rebase host path metadata for the current OS; keep model input and every
    // signed sampling payload unchanged, including their historical environment text.
    let mut records = fixture
        .lines()
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<serde_json::Result<Vec<_>>>()?;
    for record in &mut records {
        if matches!(
            record["type"].as_str(),
            Some("session_meta" | "turn_context")
        ) {
            record["payload"]["cwd"] = serde_json::json!(home.path());
        }
        if record["type"] == "turn_context" {
            record["payload"]["workspace_roots"] = serde_json::json!([home.path()]);
        }
    }
    let archive = records
        .iter()
        .map(serde_json::to_string)
        .collect::<serde_json::Result<Vec<_>>>()?
        .join("\n");
    std::fs::write(&rollout, format!("{archive}\n"))?;
    let test = spine_test_codex()
        .with_model("gpt-5.4")
        .resume(&server, home, rollout)
        .await?;
    test.submit_turn("Continue the preserved legacy migration branch.")
        .await?;
    let request = response.single_request().body_json().to_string();
    assert!(
        request.contains("Preserve this historical user request"),
        "{request}"
    );
    assert!(request.contains("legacy migration branch"), "{request}");
    assert!(
        request.contains("Continue the preserved legacy migration branch."),
        "{request}"
    );
    assert!(request.contains("<spine_node"), "{request}");
    test.codex.shutdown_and_wait().await?;
    Ok(())
}
