# Persisted rollout payloads

`RolloutItem` keeps the existing `type` / `payload` JSON shape and direct tagged
wire decoder. With `serde_json/arbitrary_precision`, Serde's internally tagged
and flatten buffering represents decimal tokens as private Number maps that
ordinary typed `f64` and `f32` fields cannot read.

The protocol's typed float fields (`RateLimitWindow.used_percent`,
`SecurityRiskScore.scores`, `ReviewOutputEvent.overall_confidence_score`, and
`ReviewFinding.confidence_score`) therefore deserialize through `serde_json::Number`.
The f64 adapter converts to finite `f64`; the f32 adapter passes the numeric token
through serde_json's native f32 decoder, retaining its range and rounding behavior.
This covers current persisted typed float fields, including alternate decimal/exponent
spellings, without materializing a whole JSON payload or changing opaque values.
Future persisted floats must use the same decoding rule. Serializer, schema,
and opaque JSON integer precision are unchanged. Malformed typed fields remain
errors; valid records are not filtered or changed to make decoding succeed.

Regression coverage is in rollout's `src/decimal_rollout_tests.rs` and thread-store's
`complete_history_preserves_decimal_rate_limits_and_security_scores`; the latter
exercises both legacy and paginated production readers, including positive and
negative integers outside the u64/i64 range in opaque action and Spine JSON.
Upstream no longer implements `Deserialize` for the flattened `RolloutLine`:
all JSONL readers use rollout's canonical `parse_rollout_line` /
`parse_rollout_line_bytes` entry points. The Value-taking `decode_rollout_line`
extracts timestamp and ordinal at the JSON boundary, then re-encodes only the
item before decoding: direct `from_value` would otherwise send wide integers
through unsupported Serde i128/u128 buffer paths. Neither path rewrites a whole
history. Complete paginated reads resolve lineage from the already selected
rollout path, preserving archived/current selection and the source metadata.
Spine archive records do not select the resumed multi-agent runtime version;
that value is still taken only from turn contexts or compaction resume metadata.
Complete reads use the same before/after store revision check as native model-context
reads. A revision is returned only when the selected rollout stayed unchanged
through the read; the complete replay history itself is never bounded at a compaction.

The canonical parser follows upstream's Value boundary: duplicate object keys
resolve to the last value before typed decoding. Retain this native contract
rather than restoring the deleted flattened Deserialize API.
The former duplicate-payload negative regression is retained as a positive behavior
test with distinct payloads and a full decoded-object comparison proving last-value
selection. This is a documented non-ledger contract change, not a silent deletion.
Review confidence roundtrips are also checked through RolloutItem, RolloutLine,
and both legacy and paginated complete-history consumers.
Run with the feature that exposes the bug:

```sh
just test --locked -p codex-rollout -p codex-thread-store \
  --features serde_json/arbitrary_precision -E 'test(decimal_)' --retries 0
```
