# SpineCodex 0.3.3

SpineCodex 0.3.3 is a compatibility patch release for resuming paginated
sessions.

## Changes

- Skip individual malformed rollout records during complete paginated lineage
  replay, matching Codex scanner behavior while retaining fatal handling for
  file I/O and lineage boundary errors.
- Add regression coverage for malformed rate-limit records.

Compatibility baseline: upstream `rust-v0.147.0`
(`be6e8eac029b183056b7e4402879f15d2c85f61b`).
