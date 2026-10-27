use super::user_agent_with_version;
use pretty_assertions::assert_eq;

#[test]
fn compatibility_user_agent_uses_upstream_version_segment() {
    let product_version = env!("CARGO_PKG_VERSION");
    assert_eq!(
        user_agent_with_version(
            format!(
                "client/{product_version} (Linux 6.8.0; x86_64) codex_cli_rs/{product_version}"
            ),
            "0.147.0",
        ),
        format!("client/0.147.0 (Linux 6.8.0; x86_64) codex_cli_rs/{product_version}"),
    );
}

#[test]
fn compatibility_user_agent_preserves_unknown_formats() {
    assert_eq!(
        user_agent_with_version("custom-client".to_string(), "0.147.0"),
        "custom-client",
    );
}
