use std::path::Path;

pub(crate) const SPINE_JIT_INSTRUCTIONS: &str = r#"<spine_view>
Spine is the primary control frame for nontrivial work: task management,
attention allocation, context compaction, resume quality, and cost control.

Use the current Spine node as the active task boundary. Nontrivial work includes
repository inspection, tools, edits, tests, commits, long reasoning, research,
or multi-turn task state. Trivial one-turn replies can be answered directly.

Before nontrivial work, choose the matching boundary:

- Continue in the current node when it represents the next work at the right
  granularity. Suitability is determined by scope and phase; shared request,
  issue, milestone, or feature is only weak evidence.
- Use `open` for a focused boundary before work grows, or for a focused
  subproblem inside the current phase. Close the child when resolved.
- Use `next` when moving from a completed phase to a peer phase.
- Use `close` when the current scope should be compacted now and no peer phase
  needs to start immediately.

`open` creates a child. `close` and `next` compact the useful state from the
current working context and reduce future prompt context. Do not wait for
perfect completion if the current working context is becoming noisy,
repetitive, or dominated by stale exploration; close or next early and keep
the useful state. The compact memory is authored by you in the `memory`
argument of `close`/`next`; runtime preserves exact user messages and child
memories, then appends your Node Memory body. Choose based on the semantic
boundary and context hygiene, not raw context size alone.
If the child already has a user-relevant conclusion, surface it before closing.

Conventions:
- `summary` is a short user-facing label in the conversation language.
- `memory` on `close`/`next` is required and must contain only the Node Memory
  body: stable continuation facts, decisions, evidence, constraints,
  unresolved risks, and next actions.
- Do not include runtime-owned headings such as `# Spine Memory`,
  `## User Message`, `## Child Memory`, or `## Node Memory` inside `memory`.
- `<spine_status>` gives cursor orientation.
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
