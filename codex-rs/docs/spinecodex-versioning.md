# SpineCodex Version Identities

SpineCodex carries two intentionally separate version identities:

- The product version is the workspace package version. It is the value shown
  by `codex --version` and used by npm packages, GitHub release tags, update
  checks, and product telemetry. The current product version is `0.1.1`.
- The Codex compatibility version is the upstream client baseline used by
  protocol-facing requests. It is recorded in
  `[workspace.metadata.spinecodex]` in `codex-rs/Cargo.toml` and projected by
  `codex-protocol` at build time. The current baseline is `0.144.6`, tag
  `rust-v0.144.6`, commit
  `5d1fbf26c43abc65a203928b2e31561cb039e06d`.

The compatibility version is used for the server-visible Codex identity in:

- the `/models?client_version=...` query and its cache identity;
- the `codex_cli_rs/<version>` User-Agent prefix;
- the built-in OpenAI provider `version` request header; and
- App Server initialize responses sent to external clients that implement the
  official upstream Codex app-server contract.

The App Server daemon is the sole initialize exception. It parses the response
User-Agent as the running SpineCodex product version and compares that value
with the managed binary's `--version`, so its probe receives the product
identity. MCP and other local product identities also continue to use the
product version.

In code, `get_codex_product_user_agent()` is product-facing and
`get_codex_compat_user_agent()` is used by upstream Codex compatibility
surfaces, including remote Codex/OpenAI HTTP requests and external App Server
clients. Keep these call sites explicit when adding a new integration.

Other Cargo-version consumers remain product-facing unless a protocol contract
explicitly classifies them as compatibility fields. Do not change the
workspace package version to follow an upstream rebase: that would change the
SpineCodex release identity and make the subscription backend evaluate the
product version as an upstream Codex client version.

When rebasing on upstream Codex, update the three metadata values together and
run the focused provider, models-manager, login, and protocol checks. A remote
`requires a newer version of Codex` response means a server-visible
compatibility field is stale; it is not evidence that npm or GitHub product
versioning should be changed.
