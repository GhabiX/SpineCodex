/// The Spine Codex product version shown in the TUI, independent of protocol compatibility.
pub const CODEX_CLI_VERSION: &str = codex_install_context::distribution::PRODUCT_VERSION;

/// Version negotiated with native Codex services, independent of the product release.
pub const CODEX_CLIENT_VERSION: &str = codex_install_context::distribution::CODEX_COMPAT_VERSION;
