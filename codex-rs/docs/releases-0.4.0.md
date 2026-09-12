# SpineCodex 0.4.0

Updates the upstream Codex baseline to `0.153.4` (`rust-v0.153.4`, commit
`3d2ee51ca2d5db578f328aa75e20aa22c0197c9a`).

- Adapt Spine sampling, recursive Spawn, tree presentation and feedback to the new baseline.
- Preserve response metadata and resolved Spine SDK configuration across sampling and resume, including when external SDK configuration files change or disappear.
- Preserve settled Spine memory while applying native child-history filtering to ordinary agent forks.
- Fix concurrent Spawn startup when paginated history contains decimal rate-limit values.
- Preserve bounded paginated resume when Spine JIT is disabled.
- When Spine JIT is enabled, open `/subagents` as a picker without adding status text to the transcript. Preserve the upstream status feed when Spine JIT is disabled.
- Keep settled Spine Spawn branches and their descendants hidden after resume and later picker refreshes, while preserving ordinary native agents in the picker.
- Follow upstream removal of full configuration locks. Legacy configuration-lock import/export and its compatibility options are removed; SDK configuration is persisted in sampling records.

The product/package version is `0.4.0`. The public CLI version and upstream HTTP
compatibility identity remain `0.153.4`, following the existing Spine versioning
contract.
