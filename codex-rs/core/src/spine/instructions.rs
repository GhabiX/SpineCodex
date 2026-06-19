use std::path::Path;

pub(crate) const SPINE_JIT_INSTRUCTIONS: &str = r#"<spine_view>
Spine helps organize ongoing work, keep the active context useful, compact old
work into memory, make later continuation reliable, and control cost.

Spine organizes ongoing work into task-level nodes. Treat the current node as
the place where present work happens. Manage nodes so the working history stays
useful for reasoning, tool use, compaction, and later continuation.
Treat a node as one compactible work unit: if a later model could resume from a
short memory without replaying the raw trace, that work unit is ready to close
or advance.

- Continue in the current node when it represents the next work at the right
  granularity. Suitability is determined by scope and phase; shared request,
  issue, milestone, or feature is only weak evidence.
- Use `open` when the next work is better handled in a focused child node.
- Use `close` when the current node has useful state worth preserving as memory
  and its full local history is no longer needed.
- Use `next` when the current node should be closed and the next work is better
  handled in a peer node.

Close or next especially when:
- a focused phase of work has produced useful state that memory can preserve;
- the next work is a peer phase and the current phase can be resumed from
  memory;
- the current node has become noisy, stale, repetitive, or dominated by
  irrelevant exploration, so preserving useful state and continuing from memory
  would be cleaner.

Simple one-turn replies can be answered directly without changing Spine.

`open` creates a child. `close` and `next` compact the useful state from the
current working context and reduce future prompt context. Do not wait for
perfect completion when the current useful state is already enough for clean
continuation from memory. The compact memory is authored by you in the `memory`
argument of `close`/`next`; runtime preserves exact user messages and child
memories, then appends your continuation memory. Choose based on whether the
current node remains useful for continued work, not raw context size alone.
If the child already has a user-relevant conclusion, surface it before closing.

Conventions:
- `summary` is a short user-facing label in the conversation language.
- `memory` on `close`/`next` is required. Write concise continuation memory for
  the next LLM that may resume this task: current progress, stable facts,
  decisions, evidence, constraints, unresolved risks, remaining work, and
  critical files, tests, commands, or references. When relevant preserved user
  messages have `[U#]` anchors, cite those anchors and state what was done
  after each request and whether it is completed, partial, blocked, or pending
  at node close. Also record user-visible conclusions or final replies already
  delivered, so a later continuation does not repeat them as new work.
- Before replying after `<spine_memory>` continuity or a node transition, check
  what has already been told to the user and only report new status, changes, or
  requested details.
- `<spine_status>` gives Spine node context and orientation when present.
- `<spine_memory>` provides continuity from closed work.
- Choose at most one of `open`, `close`, or `next` in one assistant response.
- `spine.tree` is a read-only inspector for unclear tree/cursor state.

</spine_view>
"#;

pub(crate) const SPINE_TRIM_INSTRUCTIONS: &str = r#"<spine_trim>
`spine.trim` is optional conservative cleanup for tagged tool responses from
the previous completed toolcall only. First use the previous tool result for
the active task. If a previous tool response starts with `[TRIM_ID: ...]`, you
may use `spine.trim` only when, after considering the main task, you are
confident the removed content will not be needed again. Use `op: "snip"` to
replace the whole visible body with a cleared placeholder. Use `op: "slice"`
with exactly one of `head`, `tail`, or `anchor` plus `preceding`/`following`
when a retained local part is sufficient. Do not trim merely because the output
is long. Do not trim if the response may still be needed for correctness,
debugging, citations, synthesis, or verification. If trim misses, do not retry
that `TRIM_ID`.

</spine_trim>
"#;

const SPINE_VIEW_INSTRUCTIONS_OVERRIDE_FILENAME: &str = "spine_instruction.md";
const SPINE_VIEW_START_MARKERS: [&str; 2] = ["\n\n<spine_view>", "\n\n<spine_trim>"];

pub(crate) fn append_spine_view_instructions(
    mut base_instructions: String,
    spine_jit_enabled: bool,
    spine_trim_enabled: bool,
    codex_home: &Path,
    dev_debug_prompt_overrides: bool,
) -> String {
    if !spine_jit_enabled && !spine_trim_enabled {
        return base_instructions;
    }

    let instructions = if cfg!(debug_assertions) && dev_debug_prompt_overrides {
        let override_path = codex_home.join(SPINE_VIEW_INSTRUCTIONS_OVERRIDE_FILENAME);
        match std::fs::read_to_string(override_path) {
            Ok(contents) if !contents.is_empty() => contents,
            _ => joined_spine_instructions(spine_jit_enabled, spine_trim_enabled),
        }
    } else {
        joined_spine_instructions(spine_jit_enabled, spine_trim_enabled)
    };

    if base_instructions.contains(&instructions) {
        return base_instructions;
    }
    if let Some(start) = SPINE_VIEW_START_MARKERS
        .into_iter()
        .filter_map(|marker| base_instructions.rfind(marker))
        .min()
    {
        base_instructions.truncate(start);
    }

    if !base_instructions.is_empty() {
        base_instructions.push_str("\n\n");
    }
    base_instructions.push_str(&instructions);
    base_instructions
}

fn joined_spine_instructions(spine_jit_enabled: bool, spine_trim_enabled: bool) -> String {
    let mut sections = Vec::new();
    if spine_jit_enabled {
        sections.push(SPINE_JIT_INSTRUCTIONS);
    }
    if spine_trim_enabled {
        sections.push(SPINE_TRIM_INSTRUCTIONS);
    }
    sections.join("\n\n")
}

#[cfg(test)]
#[path = "instructions_tests.rs"]
mod tests;
