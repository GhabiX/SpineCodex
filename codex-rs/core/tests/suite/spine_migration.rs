//! Cold replay of an immutable archive produced and resumed by SpineCodex 0.3.3.
use anyhow::Result;
use core_test_support::responses;
use core_test_support::test_codex::spine_test_codex;
use std::sync::Arc;

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
