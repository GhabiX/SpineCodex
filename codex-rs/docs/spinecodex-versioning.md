# SpineCodex Version Identities

SpineCodex has separate release and protocol identities:

- Product release: `PRODUCT_VERSION` in install-context distribution metadata is
  `0.4.0`. npm package manifests and GitHub release tags carry the independently
  selected product release. Doctor's product diagnostics and update comparison
  use this pin, not the upstream workspace version. The release workflow validates
  pushed `v<version>` tags against this same `PRODUCT_VERSION` constant; a tag
  matching only the compatibility/workspace version is rejected.
- Codex compatibility: `0.164.0`, tag `rust-v0.164.0`, commit
  `6aaac694803004a63c6f393d153c96d22521a6b9`. The single version source is
  `[workspace.package].version` in `codex-rs/Cargo.toml`. Rust crates inherit it
  via `version.workspace = true`; install-context derives `CODEX_COMPAT_VERSION`
  from `env!("CARGO_PKG_VERSION")` and `CODEX_UPSTREAM_TAG` with `concat!`.
  `CODEX_UPSTREAM_COMMIT` in distribution metadata records the baseline commit;
  there is no second compatibility version/tag in workspace metadata.
- The Rust workspace version is not a SpineCodex release selection. Keep the
  product pin separate. Compatibility test expectations use each crate's inherited
  `CARGO_PKG_VERSION`; product expectations use `PRODUCT_VERSION`. During an
  upstream migration, update the workspace version and baseline commit, then
  synchronize only inherited local package versions in `Cargo.lock`. Third-party
  entries follow the upstream lockfile and are not version-rewritten.

Public CLI `--version` reports compatibility identity. Remote `/models` client
version, login's compatible User-Agent, and the built-in OpenAI provider version
header also follow the upstream baseline. Product-facing User-Agent construction
uses `get_codex_product_user_agent()`; external app-server initialize uses the
compatible builder. The managed-daemon probe uses product identity and must not
mistake the public CLI's compatible version for the product release.

Upstream BuildInfo can obtain a packaged version from `codex-package.json`; a
source build can show a development commit instead. Product release staging must
supply the chosen product version explicitly. The native workspace version alone
is not evidence that an npm/native package has the right release identity.

Before a release, run the identity, provider, models-manager and daemon checks,
and stage the actual npm package. `.github/scripts/test_spine_release_behavior.py`
executes the staging command and checks the emitted root metadata, keywords,
plain description and six platform aliases. Its fixtures remain under
`temp/null/npm-metadata/`; it does not publish, install or delete them. Linux
staging and syntax checks are not a substitute for six-platform native artifacts
or Windows V8 CI.

A remote `requires a newer version of Codex` response can indicate a stale
server-visible compatibility field. A subscription-model HTTP 400 still requires
an account-level smoke test; offline metadata and local wire tests cannot prove
account availability. Do not copy credentials into fixtures or logs.

The npm release jobs use `.github/scripts/spine_npm_wait.sh` to wait for registry
visibility, because a successful `npm publish` may still be processing. Before
publishing the root package, `platforms VERSION` waits until all six platform
versions appear in `versions`. After root publication, `release VERSION TAG`
waits for those six versions, the root version and the selected dist-tag to match.
Each poll reads `versions` and `dist-tags` together from the public npm registry;
missing items and query failures are logged and retried every 20 seconds. The
publish and verification jobs each have a 30-minute timeout, with no separate
retry limit in the script. Run `.github/scripts/test_spine_npm_wait.py` for local
registry-delay and publication-order checks without publishing packages.
