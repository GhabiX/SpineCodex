# Spine Close Compact Cache POC

This note records the API-only POC used to choose the `spine.close` hidden
compact request shape. The goal was to preserve prompt-cache reuse for the
historical prefix while guaranteeing the compact request cannot call tools.

## Method

The POC replayed real Codex Responses request bodies captured from:

```text
/data/swe/FramePilot/cachetree/tasks/spine_close_compact_cache_poc_20260603_0040/allowed_tools_noschema_cli_dump_vip_20260607_134523/dump/000003-1780811146118-request.json
/data/swe/FramePilot/cachetree/tasks/spine_close_compact_cache_poc_20260603_0040/allowed_tools_noschema_cli_dump_vip_20260607_134523/dump/000004-1780811161992-request.json
```

Each replay used a fresh `prompt_cache_key` and nonce-mutated content to avoid
mistaking exact-body warming for prefix reuse. The warm request was repeated to
confirm the provider cache was active, then a compact-shaped probe varied one
field at a time.

## Findings

The old hard no-tool compact envelope was cache-cold:

```json
{
  "tools": [],
  "tool_choice": "none",
  "parallel_tool_calls": false
}
```

Fresh replay evidence:

| Probe shape          | Tail role   | Tools | Parallel | text.format | Cached tokens | Result |
| -------------------- | ----------- | ----: | -------- | ----------- | ------------: | ------ |
| `tool_choice:"none"` | `system`    |     7 | true     | no          |   2304 / 2304 | low    |
| `tool_choice:"none"` | `developer` |     7 | true     | no          |   8960 / 8448 | high   |
| `tool_choice:"none"` | `developer` |     7 | true     | no          |   8960 / 8960 | high   |
| `tool_choice:"none"` | `developer` |     0 | false    | no          |         0 / 0 | cold   |
| `tool_choice:"auto"` | `system`    |     7 | true     | no          |   1920 / 1920 | low    |
| `tool_choice:"auto"` | `developer` |     7 | true     | no          |   8576 / 9088 | high   |

Key output directories:

```text
/data/swe/FramePilot/cachetree/tasks/spine_close_compact_cache_poc_20260603_0040/poc_none_tail_system_20260607_162050
/data/swe/FramePilot/cachetree/tasks/spine_close_compact_cache_poc_20260603_0040/poc_none_tail_developer_20260607_162123
/data/swe/FramePilot/cachetree/tasks/spine_close_compact_cache_poc_20260603_0040/poc_none_dev_keep_tools_parallel_20260607_162244
/data/swe/FramePilot/cachetree/tasks/spine_close_compact_cache_poc_20260603_0040/poc_none_dev_empty_tools_parallel_false_20260607_162315
/data/swe/FramePilot/cachetree/tasks/spine_close_compact_cache_poc_20260603_0040/replay_tail_system_auto_final_20260607_160512
/data/swe/FramePilot/cachetree/tasks/spine_close_compact_cache_poc_20260603_0040/replay_tail_developer_auto_final_20260607_160601
```

The tail message role is cache-relevant. A compact directive appended as
`system` partitions the cache early, even when tools and `tool_choice` match.
Moving the same directive to the ordinary developer-message lane allows the
historical prefix to cache.

Keeping the ordinary tools array and `parallel_tool_calls=true` is also
cache-relevant. Using `tool_choice:"none"` with `tools=[]` and
`parallel_tool_calls=false` reproduces the old cold compact envelope.

Responses `text.format` strict schema is cache-relevant and should not be sent
on close compact. Compact output is therefore requested as JSON in the prompt
and parsed locally.

## POC Chosen Shape

The 2026-06-07 POC chose this shape for the first close compact request:

```json
{
  "tools": ["...ordinary sampling tools..."],
  "tool_choice": "none",
  "parallel_tool_calls": true,
  "text": { "verbosity": "low" }
}
```

The compact directive was appended as a `developer` input message. No
`text.format` schema was sent.

This shape is not the old hard no-tool envelope: it keeps full ordinary tools
and the ordinary parallel setting, but `tool_choice:"none"` prevents model tool
calls. This satisfies both constraints from the POC: no callable compact tools
and high cache reuse for the historical prefix.

## Why Not `auto`

`tool_choice:"auto"` can cache when the compact tail is `developer`, but it does
not guarantee no tool calls. A production fallback that starts with `auto` and
then discards tool calls is a separate design choice and was not the POC-backed
answer to "guarantee no tool calls while keeping prefix cache."

The POC-backed shape was therefore `tool_choice:"none"` with full tools,
parallel enabled, developer tail, and no strict response schema.

## Current Runtime Note

As of 2026-06-12, close compact appends the directive as a trailing synthetic
`user` message instead of a `developer` message. This is a behavioral fix for
observed close-compact misrouting where the model answered the previous real
user request instead of returning `SPINE_SLOT` / `SPINE_NODE_MEMORY` blocks.

This keeps the cache-relevant envelope parts measured by the POC: full ordinary
tools, inherited `parallel_tool_calls`, `tool_choice:"none"`, no
`text.format`, and the same `prompt_cache_key`. It also keeps the historical
transcript prefix token-identical by appending the compact directive after
`raw_items[..source_end]`. The old POC did not measure a synthetic `user` tail,
so provider-level cached-token behavior for that tail role still needs a fresh
API replay before treating it as empirically proven.
