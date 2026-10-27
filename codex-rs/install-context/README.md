# SpineCodex distribution identity

`distribution.rs` separates the SpineCodex product identity from the upstream
protocol identity. Product-facing output uses the independently pinned
`PRODUCT_VERSION` (`0.4.0`); the upstream workspace/package version is not the
SpineCodex release version. Update this pin only when choosing a product release.
Remote compatibility follows Codex `rust-v0.163.0`, commit
`83086188ae0614bd882818ff1aa23f3194962012`.

`codex-rs/Cargo.toml`'s `workspace.package.version` is the single compatibility
version source. This crate inherits it: `CODEX_COMPAT_VERSION` uses
`env!("CARGO_PKG_VERSION")`, and `CODEX_UPSTREAM_TAG` prefixes that value with
`rust-v`. `CODEX_UPSTREAM_COMMIT` independently records the baseline commit.
No duplicate compatibility version or tag is stored in workspace metadata.

The compatibility version is used by login's HTTP User-Agent builder, the
OpenAI provider's `version` header, and models-manager's client-version argument.
Tests derive their expected compatibility version from their own inherited
`CARGO_PKG_VERSION`, rather than copying a release number or using the same
compatibility constant as the implementation. On an upstream migration, update
the workspace version and baseline commit, then synchronize inherited local
package versions in `Cargo.lock`; keep third-party lock entries unchanged.

The models-manager regression observes the compatibility-version endpoint
argument; the model-provider routing test
captures the production endpoint's transport URL, including
`/models?client_version=...`. Login tests check the actual compatibility user
agent. These local checks do not make remote account requests.
