# Upstream 0.164.0 migration

This catchup follows [the catchup SPEC](../../docs/codex-catchup.md). It replays
the committed Spine delta onto the official release in one preparation commit,
then records compatibility adaptations separately on `sync/codex-v0.164.0`.

## Provenance

| Symbol | Source | Commit |
| --- | --- | --- |
| M | Frozen main: `history/pre-codex-v0.164.0` | `9fdac81aa4c6f202ea53d0d8b4134e0c046ea773` |
| B | Previous official base: `rust-v0.163.0` | `83086188ae0614bd882818ff1aa23f3194962012` |
| U | New official base: `rust-v0.164.0` | `6aaac694803004a63c6f393d153c96d22521a6b9` |
| S | Preparation squash, with U as its sole parent | `fb9c55ae39980da829e6952349739e084d92a100` |

The preparation resolves textual conflicts while retaining upstream behavior and
Spine intent. The previous sync branch's README link and English catchup SPEC
were absent from M and are restored in a separate documentation commit. The
[0.163 migration record](spinecodex-upstream-0.163.md) remains historical evidence.

## Adaptation scope

- Boxed rollout records and cancellable, seekable readers, including compressed
  rollout offsets used by Spine sampling-boundary forks.
- Typed context fragments and harness metadata, child settings inheritance,
  message delivery/mailboxes, optional prompt-cache identity and shared tools.
- App-server/TUI integration, generated protocol and SDK exports, and native
  behavior when Spine is disabled.
- Product release workflow uses the upstream Rust toolchain and reads release
  provenance from the current Cargo/distribution identity sources.
- Workspace compatibility version `0.164.0`, its inherited lockfile entries and
  the pinned upstream commit. `PRODUCT_VERSION` remains `0.4.0` until the separate
  product release commit; compatibility identity is not a release selection.

## Verification status

Migration and validation are in progress. This note does not claim that the
candidate compiles, passes the behavioral gates, or is ready for release.
Source-bound commands, results, conflict decisions and remaining gaps are kept
in the outer CacheTree task `tasks/codex_catchup_20261027_1615/`.

Acceptance requires the SPEC's build and behavior gates, including native mode,
open/close/next/spawn, per-sampling replacement, persistence and supported
resume/fork/rollback/AoT/compact paths. Generated exports and release artifacts
must be checked against the final candidate before promotion and publication.
