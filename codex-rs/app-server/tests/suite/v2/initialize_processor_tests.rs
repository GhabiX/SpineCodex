use super::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn compatibility_user_agent_uses_upstream_version_segment() -> Result<()> {
    assert_initialize_identity(
        "codex_vscode",
        &format!("codex_vscode/{} ", env!("CARGO_PKG_VERSION")),
        true,
    )
    .await
}

#[tokio::test]
async fn daemon_user_agent_preserves_product_identity() -> Result<()> {
    assert_initialize_identity(
        "codex_app_server_daemon",
        &format!(
            "codex_cli_rs/{} ",
            codex_install_context::distribution::PRODUCT_VERSION,
        ),
        false,
    )
    .await
}

async fn assert_initialize_identity(
    client_name: &str,
    expected_prefix: &str,
    originating: bool,
) -> Result<()> {
    let server = create_mock_responses_server_sequence_unchecked(Vec::new()).await;
    let codex_home = TempDir::new()?;
    MockResponsesConfig::new(&server.uri())
        .disable_feature(Feature::ShellSnapshot)
        .write(codex_home.path())?;
    let mut app = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_env_overrides(&[("CODEX_INTERNAL_ORIGINATOR_OVERRIDE", None)])
        .build()
        .await?;
    let message = timeout(
        DEFAULT_READ_TIMEOUT,
        app.initialize_with_client_info(ClientInfo {
            name: client_name.to_string(),
            title: None,
            version: "0.1.0".to_string(),
        }),
    )
    .await??;
    let JSONRPCMessage::Response(response) = message else {
        anyhow::bail!("expected initialize response, got {message:?}");
    };
    let InitializeResponse { user_agent, .. } = to_response::<InitializeResponse>(response)?;
    assert!(
        user_agent.starts_with(expected_prefix),
        "unexpected identity: {user_agent}"
    );
    assert_eq!(
        user_agent.ends_with(&format!(" ({client_name}; 0.1.0)")),
        originating,
    );
    Ok(())
}
