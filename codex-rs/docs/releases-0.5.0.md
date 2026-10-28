# SpineCodex 0.5.0

Updates the upstream Codex baseline to `0.164.0` (`rust-v0.164.0`, commit
`6aaac694803004a63c6f393d153c96d22521a6b9`).

- Adapt Spine sampling, recursive spawn, history persistence and TUI integration
  to the new upstream APIs, including compressed rollout readers and typed
  context metadata.
- Preserve custom or omitted prompt-cache keys in spawned children. Keep native
  request identity unchanged when Spine is disabled.
- Preserve complete Spine lineage when sampling sessions after compaction.
- Follow upstream native fork prefix policies and message delivery semantics.
- Refresh protocol/SDK exports and release tooling for the upstream Rust
  toolchain. Document the six-step catchup and external contribution workflow.

The product/package version is `0.5.0`. The public CLI version and upstream HTTP
compatibility identity are `0.164.0`; these are separate version identities.
