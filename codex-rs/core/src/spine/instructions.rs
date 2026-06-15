use std::path::Path;

pub(crate) const SPINE_VIEW_INSTRUCTIONS: &str = r#"<spine_view>
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
- Use `close` when the current phase is complete and there is no peer phase to start.

`open` creates a child. `close` and `next` compact completed work and reduce
future prompt context. Choose based on the semantic boundary, not raw context
size alone.
If the child already has a user-relevant conclusion, surface it before closing.

Conventions:
- `summary` is a short user-facing label in the conversation language.
- `instruction` on `close`/`next` is optional one-sentence guidance for the
  later internal memory compaction turn; do not write memory in the tool call.
- If you see `---------- SPINE MEMORY COMPACT ----------`, runtime is running
  that internal compaction turn. Treat prior transcript as source material, not
  active dialogue, and follow the compact directive over ordinary assistant
  duties.
- `<spine_status>` gives cursor orientation.
- `<spine_memory>` provides continuity from closed work.
- Choose at most one of `open`, `close`, or `next` in one assistant response.
- `spine.tree` is a read-only inspector for unclear tree/cursor state.
- When a previous tool response starts with `[TRIM_ID: ...]` and that output is
  no longer needed verbatim, use `spine.trim` with `op: "snip"` on the next
  turn. It only clears a tagged response from the previous completed toolcall.

</spine_view>
"#;

const SPINE_VIEW_INSTRUCTIONS_OVERRIDE_FILENAME: &str = "spine_instruction.md";
const SPINE_VIEW_SEPARATOR_AND_START_MARKER: &str = "\n\n<spine_view>";

pub(crate) fn append_spine_view_instructions(
    mut base_instructions: String,
    enabled: bool,
    codex_home: &Path,
    dev_debug_prompt_overrides: bool,
) -> String {
    if !enabled {
        return base_instructions;
    }

    let instructions = if cfg!(debug_assertions) && dev_debug_prompt_overrides {
        let override_path = codex_home.join(SPINE_VIEW_INSTRUCTIONS_OVERRIDE_FILENAME);
        match std::fs::read_to_string(override_path) {
            Ok(contents) if !contents.is_empty() => contents,
            _ => SPINE_VIEW_INSTRUCTIONS.to_string(),
        }
    } else {
        SPINE_VIEW_INSTRUCTIONS.to_string()
    };

    if base_instructions.contains(&instructions) {
        return base_instructions;
    }
    if let Some(start) = base_instructions.rfind(SPINE_VIEW_SEPARATOR_AND_START_MARKER) {
        base_instructions.truncate(start);
    }

    if !base_instructions.is_empty() {
        base_instructions.push_str("\n\n");
    }
    base_instructions.push_str(&instructions);
    base_instructions
}

#[cfg(test)]
#[path = "instructions_tests.rs"]
mod tests;
