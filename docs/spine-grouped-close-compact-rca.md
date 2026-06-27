# RCA: grouped `spine.open` duplicated visible `context_index`

## Summary

The real bug is not that `ContextManager` appended two host items into the same
slot. The bug is that Spine recorded two different visible `PS` entries with
the same `context_index`.

Observed sidecar evidence:

```text
seq300 open child ... boundary raw561 index201
seq301 tool_call request raw561 ctx201 response raw562 ctx202
seq302 msg raw563 ctx202
```

`raw562` is the `spine.open` tool response. `raw563` is the next assistant
message. They are two different visible symbols in the current variable
context, so they must not share `context_index=202`.

## Evidence

Real rollout:

```text
/home/ghabi/.codex/sessions/2026/06/24/rollout-2026-06-24T15-26-52-019ef886-4f43-7401-926d-477346f6fd3b.jsonl
```

Relevant raw order:

```text
line 869 raw561 function_call name=spine.open call_id=call_EA6...
line 870 raw562 function_call_output call_id=call_EA6... output="Spine open accepted."
line 873 raw563 assistant message
```

Sidecar:

```text
/home/ghabi/.codex/sessions/2026/06/24/spine-rollout-2026-06-24T15-26-52-019ef886-4f43-7401-926d-477346f6fd3b/tree.jsonl
```

Relevant sidecar rows:

```json
{"seq":300,"type":"open","child":[1,1,2,3,3,2],"boundary":561,"index":201,"summary":"手工构造真实样题"}
{"seq":301,"type":"tool_call","segments":[{"kind":"request","raw_ordinal":561,"context_index":201},{"kind":"response","raw_ordinal":562,"context_index":202}]}
{"seq":302,"type":"msg","raw_ordinal":563,"context_index":202,"from_user":false}
```

`ContextManager::record_items` assigns `context_index = self.items.len()` and
then pushes exactly one API-visible item. That append path cannot by itself make
two adjacent visible host items share one index.

## Formal invariant

`PS` is the current variable context state. For visible response-item segments
inside `h(PS)`, `context_index` is the coordinate in the current variable host
context.

Therefore:

```text
For visible SegRef::ResponseItem entries in PS order:
context_index must be strictly increasing.
```

Closed child trees are memory nodes and are not expanded as current visible raw
response segments. The invariant applies to response-item refs that are visible
in the current `PS -> h(PS)` projection.

## Root cause

`spine.open` itself is only a control tool. It does not directly shift an
`open` token. The parser-effective behavior happens after the completed
toolcall hook:

```text
toolreq toolresp+
  -> lexer emits: open toolcall
  -> parser consumes: shift open, then shift toolcall
```

Grouped toolcalls add one implementation wrinkle. The grouped output path must
pre-record the grouped tool outputs into host history so the runtime can form a
completed `toolcall` evidence object. That pre-recording calculates output
coordinates from the current host length:

```text
output_context_start = current_history.len()
```

In the failing run, that gave the `spine.open` output `ctx202`.

The old grouped-open commit path then allowed parser/sidecar state and host
publication to diverge:

- it prepared `open + toolcall` using evidence carrying pre-recorded host
  coordinates;
- the final host variable context was not published from the same final
  parser state used by the sidecar;
- the following assistant message was later observed at the same visible
  context coordinate.

So the duplicate `ctx202` was a bridge bug between:

```text
completed grouped toolcall evidence
-> parser shift/reduce
-> sidecar SegRef.context_index
-> host history publication
```

It was not a normal property of raw provenance, and it was not acceptable for
`PS`.

## Correct fix

The repair has two parts.

1. Treat grouped `spine.open` as one prepared parser transaction.

   The grouped open commit must stage the final parser state for:

   ```text
   open toolcall
   ```

   It must not install an intermediate open state and then rely on stale host
   coordinates for the toolcall leaf.

2. Publish host history from the final prepared `PS`.

   When a prepared commit has a `ParserCommitInstall`, the publication path must
   materialize:

   ```text
   final PS -> h(PS)
   ```

   and use that materialized context as the host replacement. The parser state
   installed after host publication and the history visible to the model must
   be the same state.

Additionally, `ParseStack::shift` now fail-fast validates that any newly shifted
visible response refs are strictly after the last visible response ref already
in `PS`. This turns future duplicate-coordinate bugs into immediate parser
errors instead of silently corrupting sidecar state.

## Regression coverage

The regression test:

```text
grouped_spine_open_output_and_followup_message_have_distinct_context_slots
```

checks both required properties:

- host history contains `open request`, `open output`, then the follow-up
  assistant message in that order;
- replayed Spine `PS` visible response refs have strictly increasing
  `context_index`;
- the grouped open output and the follow-up assistant message do not share a
  visible `context_index`.

Related grouped-open-after-close coverage:

```text
grouped_spine_open_after_close_uses_rollout_raw_evidence_for_projection
```

continues to pass.

## Files

- `codex-rs/core/src/spine/runtime/commit.rs`
- `codex-rs/core/src/spine/runtime/prepared.rs`
- `codex-rs/core/src/spine/parser.rs`
- `codex-rs/core/src/spine/parse_stack.rs`
- `codex-rs/core/src/spine/runtime.rs`
- `codex-rs/core/src/session/tests.rs`
