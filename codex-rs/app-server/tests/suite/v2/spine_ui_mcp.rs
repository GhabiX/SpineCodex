use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::Result;
use app_test_support::TestAppServer;
use app_test_support::create_mock_responses_server_sequence_unchecked;
use app_test_support::to_response;
use app_test_support::write_mock_responses_config_toml;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::ListMcpServerStatusParams;
use codex_app_server_protocol::ListMcpServerStatusResponse;
use codex_app_server_protocol::McpResourceContent;
use codex_app_server_protocol::McpResourceReadParams;
use codex_app_server_protocol::McpResourceReadResponse;
use codex_app_server_protocol::McpServerToolCallParams;
use codex_app_server_protocol::McpServerToolCallResponse;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_core::config::set_project_trust_level;
use codex_protocol::config_types::TrustLevel;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(10);
const INTERNAL_SERVER: &str = "__codex_internal_spine_tree_ui__";
const INTERNAL_TOOL: &str = "spine_tree";
const INTERNAL_RESOURCE_URI: &str = "ui://spine/tree.html";

#[tokio::test]
async fn internal_spine_ui_mcp_surface_is_available_only_when_enabled() -> Result<()> {
    let (_codex_home, mut mcp) = start_app_server(Some("1")).await?;

    let status_id = mcp
        .send_list_mcp_server_status_request(ListMcpServerStatusParams {
            cursor: None,
            limit: None,
            detail: None,
            thread_id: None,
        })
        .await?;
    let status: ListMcpServerStatusResponse = to_response(
        timeout(
            DEFAULT_READ_TIMEOUT,
            mcp.read_stream_until_response_message(RequestId::Integer(status_id)),
        )
        .await??,
    )?;
    assert_eq!(status.data.len(), 1);
    assert_eq!(status.data[0].name, INTERNAL_SERVER);
    assert!(status.data[0].tools.contains_key(INTERNAL_TOOL));
    assert!(
        status.data[0]
            .resources
            .iter()
            .any(|resource| resource.uri == INTERNAL_RESOURCE_URI)
    );

    let resource_id = mcp
        .send_mcp_resource_read_request(McpResourceReadParams {
            thread_id: None,
            server: INTERNAL_SERVER.to_string(),
            uri: INTERNAL_RESOURCE_URI.to_string(),
        })
        .await?;
    let resource: McpResourceReadResponse = to_response(
        timeout(
            DEFAULT_READ_TIMEOUT,
            mcp.read_stream_until_response_message(RequestId::Integer(resource_id)),
        )
        .await??,
    )?;
    let McpResourceContent::Text {
        mime_type, text, ..
    } = &resource.contents[0]
    else {
        panic!("expected internal Spine UI HTML resource")
    };
    assert_eq!(mime_type.as_deref(), Some("text/html;profile=mcp-app"));
    assert!(text.contains("<!doctype html>"));

    let thread_id = start_thread(&mut mcp).await?;
    let tool_id = mcp
        .send_mcp_server_tool_call_request(McpServerToolCallParams {
            thread_id: thread_id.clone(),
            server: INTERNAL_SERVER.to_string(),
            tool: INTERNAL_TOOL.to_string(),
            arguments: Some(json!({})),
            meta: None,
        })
        .await?;
    let tool: McpServerToolCallResponse = to_response(
        timeout(
            DEFAULT_READ_TIMEOUT,
            mcp.read_stream_until_response_message(RequestId::Integer(tool_id)),
        )
        .await??,
    )?;
    assert_eq!(tool.is_error, Some(true));
    assert_eq!(tool.structured_content, None);
    assert_eq!(
        tool.content[0].get("text"),
        Some(&json!("No Spine Tree is active for this thread."))
    );

    let invalid_tool_id = mcp
        .send_mcp_server_tool_call_request(McpServerToolCallParams {
            thread_id,
            server: INTERNAL_SERVER.to_string(),
            tool: INTERNAL_TOOL.to_string(),
            arguments: Some(json!({"unexpected": true})),
            meta: None,
        })
        .await?;
    let error = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(invalid_tool_id)),
    )
    .await??;
    assert!(error.error.message.contains("empty object"));
    Ok(())
}

#[tokio::test]
async fn disabled_internal_spine_ui_mcp_surface_falls_through_to_normal_mcp() -> Result<()> {
    let (_codex_home, mut mcp) = start_app_server(Some("off")).await?;

    let status_id = mcp
        .send_list_mcp_server_status_request(ListMcpServerStatusParams {
            cursor: None,
            limit: None,
            detail: None,
            thread_id: None,
        })
        .await?;
    let status: ListMcpServerStatusResponse = to_response(
        timeout(
            DEFAULT_READ_TIMEOUT,
            mcp.read_stream_until_response_message(RequestId::Integer(status_id)),
        )
        .await??,
    )?;
    assert_eq!(status.data, Vec::new());

    let resource_id = mcp
        .send_mcp_resource_read_request(McpResourceReadParams {
            thread_id: None,
            server: INTERNAL_SERVER.to_string(),
            uri: INTERNAL_RESOURCE_URI.to_string(),
        })
        .await?;
    let resource_error: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(resource_id)),
    )
    .await??;
    assert!(resource_error.error.message.contains(INTERNAL_SERVER));

    let thread_id = start_thread(&mut mcp).await?;
    let tool_id = mcp
        .send_mcp_server_tool_call_request(McpServerToolCallParams {
            thread_id,
            server: INTERNAL_SERVER.to_string(),
            tool: INTERNAL_TOOL.to_string(),
            arguments: Some(json!({})),
            meta: None,
        })
        .await?;
    let tool_error: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(tool_id)),
    )
    .await??;
    assert!(tool_error.error.message.contains(INTERNAL_SERVER));
    Ok(())
}

#[tokio::test]
async fn enabled_internal_spine_ui_mcp_rejects_a_configured_name_collision() -> Result<()> {
    let responses_server = create_mock_responses_server_sequence_unchecked(Vec::new()).await;
    let codex_home = TempDir::new()?;
    write_mock_responses_config_toml(
        codex_home.path(),
        &responses_server.uri(),
        &BTreeMap::new(),
        /*auto_compact_limit*/ 1024,
        /*requires_openai_auth*/ None,
        "mock_provider",
        "compact",
    )?;
    let config_path = codex_home.path().join("config.toml");
    let mut config_toml = std::fs::read_to_string(&config_path)?;
    config_toml.push_str(&format!(
        "\n[mcp_servers.{INTERNAL_SERVER}]\ncommand = \"missing-spine-ui-collision-command\"\n"
    ));
    std::fs::write(config_path, config_toml)?;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_env_overrides(&[("CODEX_SPINE_APP_UI", Some("1"))])
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let status_id = mcp
        .send_list_mcp_server_status_request(ListMcpServerStatusParams {
            cursor: None,
            limit: None,
            detail: None,
            thread_id: None,
        })
        .await?;
    let error = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(status_id)),
    )
    .await??;
    assert!(error.error.message.contains("reserved"));

    let resource_id = mcp
        .send_mcp_resource_read_request(McpResourceReadParams {
            thread_id: None,
            server: INTERNAL_SERVER.to_string(),
            uri: INTERNAL_RESOURCE_URI.to_string(),
        })
        .await?;
    let error = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(resource_id)),
    )
    .await??;
    assert!(error.error.message.contains("reserved"));

    let thread_id = start_thread(&mut mcp).await?;
    let tool_id = mcp
        .send_mcp_server_tool_call_request(McpServerToolCallParams {
            thread_id: thread_id.clone(),
            server: INTERNAL_SERVER.to_string(),
            tool: INTERNAL_TOOL.to_string(),
            arguments: Some(json!({})),
            meta: None,
        })
        .await?;
    let error = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(tool_id)),
    )
    .await??;
    assert!(error.error.message.contains("reserved"));
    Ok(())
}

#[tokio::test]
async fn enabled_internal_spine_ui_mcp_rejects_a_project_local_name_collision() -> Result<()> {
    let (codex_home, mut mcp) = start_app_server(Some("1")).await?;
    let workspace = TempDir::new()?;
    std::fs::create_dir_all(workspace.path().join(".git"))?;
    set_project_trust_level(codex_home.path(), workspace.path(), TrustLevel::Trusted)?;

    let thread_id = start_thread_in_cwd(&mut mcp, workspace.path().display().to_string()).await?;
    let project_config_dir = workspace.path().join(".codex");
    std::fs::create_dir_all(&project_config_dir)?;
    std::fs::write(
        project_config_dir.join("config.toml"),
        format!(
            "[mcp_servers.{INTERNAL_SERVER}]\ncommand = \"missing-project-spine-ui-collision-command\"\n"
        ),
    )?;

    let threadless_resource_id = mcp
        .send_mcp_resource_read_request(McpResourceReadParams {
            thread_id: None,
            server: INTERNAL_SERVER.to_string(),
            uri: INTERNAL_RESOURCE_URI.to_string(),
        })
        .await?;
    let _: McpResourceReadResponse = to_response(
        timeout(
            DEFAULT_READ_TIMEOUT,
            mcp.read_stream_until_response_message(RequestId::Integer(threadless_resource_id)),
        )
        .await??,
    )?;

    let status_id = mcp
        .send_list_mcp_server_status_request(ListMcpServerStatusParams {
            cursor: None,
            limit: None,
            detail: None,
            thread_id: Some(thread_id.clone()),
        })
        .await?;
    let error = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(status_id)),
    )
    .await??;
    assert!(error.error.message.contains("reserved"));

    let resource_id = mcp
        .send_mcp_resource_read_request(McpResourceReadParams {
            thread_id: Some(thread_id.clone()),
            server: INTERNAL_SERVER.to_string(),
            uri: INTERNAL_RESOURCE_URI.to_string(),
        })
        .await?;
    let error = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(resource_id)),
    )
    .await??;
    assert!(error.error.message.contains("reserved"));

    let tool_id = mcp
        .send_mcp_server_tool_call_request(McpServerToolCallParams {
            thread_id,
            server: INTERNAL_SERVER.to_string(),
            tool: INTERNAL_TOOL.to_string(),
            arguments: Some(json!({})),
            meta: None,
        })
        .await?;
    let error = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(tool_id)),
    )
    .await??;
    assert!(error.error.message.contains("reserved"));
    Ok(())
}

async fn start_app_server(spine_ui_env: Option<&str>) -> Result<(TempDir, TestAppServer)> {
    let responses_server = create_mock_responses_server_sequence_unchecked(Vec::new()).await;
    let codex_home = TempDir::new()?;
    write_mock_responses_config_toml(
        codex_home.path(),
        &responses_server.uri(),
        &BTreeMap::new(),
        /*auto_compact_limit*/ 1024,
        /*requires_openai_auth*/ None,
        "mock_provider",
        "compact",
    )?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_env_overrides(&[("CODEX_SPINE_APP_UI", spine_ui_env)])
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;
    Ok((codex_home, mcp))
}

async fn start_thread(mcp: &mut TestAppServer) -> Result<String> {
    start_thread_with_params(
        mcp,
        ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        },
    )
    .await
}

async fn start_thread_in_cwd(mcp: &mut TestAppServer, cwd: String) -> Result<String> {
    start_thread_with_params(
        mcp,
        ThreadStartParams {
            model: Some("mock-model".to_string()),
            cwd: Some(cwd),
            ..Default::default()
        },
    )
    .await
}

async fn start_thread_with_params(
    mcp: &mut TestAppServer,
    params: ThreadStartParams,
) -> Result<String> {
    let request_id = mcp.send_thread_start_request(params).await?;
    let response: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response(response)?;
    Ok(thread.id)
}
