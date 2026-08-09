use anyhow::Result;
use app_test_support::DEFAULT_CLIENT_NAME;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use app_test_support::create_mock_responses_server_repeating_assistant;
use codex_app_server_protocol::ClientInfo;
use codex_app_server_protocol::InitializeCapabilities;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadForkParams;
use codex_app_server_protocol::ThreadForkResponse;
use codex_app_server_protocol::ThreadResumeParams;
use codex_app_server_protocol::ThreadResumeResponse;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::ThreadStartedNotification;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::UserInput;
use codex_features::Feature;
use codex_feedback::SPINE_FEEDBACK_MAX_NOTE_BYTES;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

async fn build_app_server_with_experimental_api(
    codex_home: &std::path::Path,
    experimental_api: bool,
) -> Result<TestAppServer> {
    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home)
        .build()
        .await?;
    app_server
        .initialize_with_capabilities(
            ClientInfo {
                name: DEFAULT_CLIENT_NAME.to_string(),
                title: None,
                version: "0.1.0".to_string(),
            },
            Some(InitializeCapabilities {
                experimental_api,
                ..Default::default()
            }),
        )
        .await?;
    Ok(app_server)
}

async fn assert_lifecycle_feedback_capability(
    codex_home: &std::path::Path,
    experimental_api: bool,
) -> Result<()> {
    let mut app_server =
        build_app_server_with_experimental_api(codex_home, experimental_api).await?;
    let start_request_id = app_server
        .send_thread_start_request(ThreadStartParams::default())
        .await?;
    let started: ThreadStartResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        app_server.read_response(start_request_id),
    )
    .await??;
    let expected = experimental_api.then_some(true);
    assert_eq!(started.spine_feedback_enabled, expected);
    let _: ThreadStartedNotification = timeout(
        DEFAULT_READ_TIMEOUT,
        app_server.read_notification("thread/started"),
    )
    .await??;

    timeout(
        DEFAULT_READ_TIMEOUT,
        app_server.start_turn_and_wait_for_completion(TurnStartParams {
            thread_id: started.thread.id.clone(),
            input: vec![UserInput::Text {
                text: "persist lifecycle fixture".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        }),
    )
    .await??;

    let resume_request_id = app_server
        .send_thread_resume_request(ThreadResumeParams {
            thread_id: started.thread.id.clone(),
            ..Default::default()
        })
        .await?;
    let resumed: ThreadResumeResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        app_server.read_response(resume_request_id),
    )
    .await??;
    assert_eq!(resumed.spine_feedback_enabled, expected);

    let fork_request_id = app_server
        .send_thread_fork_request(ThreadForkParams {
            thread_id: started.thread.id,
            ..Default::default()
        })
        .await?;
    let forked: ThreadForkResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        app_server.read_response(fork_request_id),
    )
    .await??;
    assert_eq!(forked.spine_feedback_enabled, expected);
    Ok(())
}

#[tokio::test]
async fn spine_feedback_capability_is_present_for_opted_in_lifecycle_responses() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("unused").await;
    let codex_home = TempDir::new()?;
    MockResponsesConfig::new(&server.uri())
        .enable_feature(Feature::SpineJit)
        .disable_feature(Feature::SpineTrim)
        .disable_feature(Feature::SpineSpawn)
        .write(codex_home.path())?;

    assert_lifecycle_feedback_capability(codex_home.path(), true).await
}

#[tokio::test]
async fn spine_feedback_capability_is_omitted_for_stable_lifecycle_responses() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("unused").await;
    let codex_home = TempDir::new()?;
    MockResponsesConfig::new(&server.uri())
        .enable_feature(Feature::SpineJit)
        .disable_feature(Feature::SpineTrim)
        .disable_feature(Feature::SpineSpawn)
        .write(codex_home.path())?;

    assert_lifecycle_feedback_capability(codex_home.path(), false).await
}

#[tokio::test]
async fn spine_feedback_upload_rejects_a_non_spine_thread_over_json_rpc() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("unused").await;
    let codex_home = TempDir::new()?;
    MockResponsesConfig::new(&server.uri())
        .disable_feature(Feature::SpineJit)
        .disable_feature(Feature::SpineTrim)
        .disable_feature(Feature::SpineSpawn)
        .write(codex_home.path())?;
    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build_initialized()
        .await?;

    let start_request_id = app_server
        .send_thread_start_request_with_auto_env(ThreadStartParams::default())
        .await?;
    let started: ThreadStartResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        app_server.read_response(start_request_id),
    )
    .await??;
    assert_eq!(started.spine_feedback_enabled, Some(false));

    for screenshots in [
        None,
        Some(serde_json::Value::Null),
        Some(json!([])),
        Some(json!([{"pngBase64": "cG5n"}])),
    ] {
        let mut params = json!({
            "threadId": started.thread.id,
            "note": "feedback",
        });
        if let Some(screenshots) = screenshots {
            params["screenshots"] = screenshots;
        }
        let feedback_request_id = app_server
            .send_raw_request("feedback/spineUpload", Some(params))
            .await?;
        let error: JSONRPCError = timeout(
            DEFAULT_READ_TIMEOUT,
            app_server.read_stream_until_error_message(RequestId::Integer(feedback_request_id)),
        )
        .await??;
        assert_eq!(error.error.code, -32600);
        assert_eq!(
            error.error.message,
            "feedback/spineUpload requires a Spine-enabled thread"
        );
    }

    Ok(())
}

#[tokio::test]
async fn spine_feedback_upload_enforces_bounds_after_spine_capability_is_advertised() -> Result<()>
{
    let server = create_mock_responses_server_repeating_assistant("unused").await;
    let codex_home = TempDir::new()?;
    MockResponsesConfig::new(&server.uri())
        .enable_feature(Feature::SpineJit)
        .disable_feature(Feature::SpineTrim)
        .disable_feature(Feature::SpineSpawn)
        .write(codex_home.path())?;
    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build_initialized()
        .await?;

    let start_request_id = app_server
        .send_thread_start_request_with_auto_env(ThreadStartParams::default())
        .await?;
    let started: ThreadStartResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        app_server.read_response(start_request_id),
    )
    .await??;
    assert_eq!(started.spine_feedback_enabled, Some(true));

    let feedback_request_id = app_server
        .send_raw_request(
            "feedback/spineUpload",
            Some(json!({
                "threadId": started.thread.id,
                "note": "x".repeat(SPINE_FEEDBACK_MAX_NOTE_BYTES + 1),
                "screenshots": [],
            })),
        )
        .await?;
    let error: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        app_server.read_stream_until_error_message(RequestId::Integer(feedback_request_id)),
    )
    .await??;
    assert_eq!(error.error.code, -32600);
    assert_eq!(
        error.error.message,
        format!("Spine feedback note exceeds {SPINE_FEEDBACK_MAX_NOTE_BYTES} bytes")
    );

    Ok(())
}
