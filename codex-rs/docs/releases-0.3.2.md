# SpineCodex 0.3.2

SpineCodex 0.3.2 is a compatibility and usability patch release.

## Changes

- Restored the upstream Codex compatibility identity (`0.147.0`) for Responses
  `version` headers, `/models` `client_version`, and remote User-Agent headers.
  Product version `0.3.2` remains separate. This addresses server-side
  compatibility errors such as `gpt-5.6-luna requires a newer version of Codex`.
- Applied the compatibility User-Agent to backend-client and cloud-tasks remote
  calls as well.
- Preserved the isolated `spine-codex-version.json` update cache.
- Hid closed Spawn agents after resume so the subagent picker contains only
  currently loaded agents.
- Added release-contract, product-documentation, and compatibility metadata
  consistency gates before native builds.

Compatibility baseline: upstream `rust-v0.147.0`
(`be6e8eac029b183056b7e4402879f15d2c85f61b`).
