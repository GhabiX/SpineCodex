use std::path::Path;

pub(crate) const SPINE_VIEW_INSTRUCTIONS: &str = r#"<spine_view>
Spine is the core control frame for managing context, attention, compact memory, and token cost while completing any task. Treat the current Spine tree as the primary structure for organizing work, preserving completed context, and deciding when a focused child, sibling phase, or close/compact boundary will help. Treat each new request as potentially complex until the work proves otherwise. Use the current Spine tree early to keep task-specific context under an appropriate node instead of letting it accumulate in an umbrella/root node. For substantive or uncertain work, establish one clear task or phase node near the start; once inside a suitable node, continue there until the work reaches a real phase boundary.

Mental model:
- Current node = live scratchpad for one coherent phase.
- Closed node = runtime-generated compact memory for future orientation.
- spine.open = enter a focused child for a substantial blocker or dependency.
- spine.next = finish this phase and start a sibling phase.
- spine.close = finish this scope and return to its parent.

Context savings happen only when a live node is closed: `spine.close` and the close step of `spine.next` replace that node's raw history with compact memory in future prompts. `spine.open` only creates a child boundary for narrower work; it does not shrink the current context window by itself.

Treat `<spine_status .../>`, pressure hints, and Spine tool outputs as temporary orientation for the current cursor and boundary choice. Treat `<spine_memory ...>` as compact memory from previously closed work for orientation, not as a new user request; task summaries should preserve task facts, decisions, evidence, risks, and resume focus.

Tools:
- spine.tree: inspect the tree, cursor, and context pressure when that state is unclear.
- spine.open(summary): open a focused child under the current node when the phase is likely to benefit from later compaction. Write summary as a short motivation/boundary label.
- spine.close(instruction?): close the current node, compact its raw history into memory, and resume the parent.
- spine.next(summary, instruction?): close the current node, preserve compact guidance as memory, then continue in a new sibling under the resumed parent. Write summary with the same rule as spine.open: label why the next sibling phase exists now, not a recap of the node being closed.

Let task structure emerge from evidence and natural boundaries. After spine.open/close/next, use the returned Spine projection as the source of truth for cursor, parent, and siblings.

Scope discipline: keep umbrella/parent nodes for coordination, not as long work buffers. For substantive research, implementation, verification, or long task-specific discussion, work inside a focused child. Do not repeatedly open nested children for the same phase; continue in the current focused node unless there is a real narrower blocker. Use `spine.next` for peer phases such as research -> implementation -> verification, and `spine.close` when a focused scope is complete. Do not split Spine for individual commands, checklist items, or tiny updates.

Move Spine by matching the next work to the tree:
- Same scope: continue in the current node.
- Narrower blocker or dependency: open a child whose result helps the parent continue.
- Peer phase: use spine.next to close the current phase and continue as a sibling.
- Finished scope: close with one sentence naming what later work should remember.
- Material user redirect: close or advance the stale phase before continuing.
- Settled facts are being repeated or re-audited: close or advance first.

Good Spine boundaries follow the work: research, design, implementation, verification, review, synthesis, repeated passes, and user redirects usually form sibling phases; focused investigations or blockers are children. As context grows, prefer the next coherent handoff so completed detail becomes memory before the live node gets too large.

Examples:
- Research -> plan -> implementation -> verification are peer phases; use spine.next between them.
- Implementation -> investigate one failing test is a nested blocker; use spine.open for the investigation.
- If the user says "first make a POC" after you started implementing, close or advance into the POC phase before acting.

For spine.close and spine.next, pass only one short compact-guidance sentence about what future work should preserve; runtime writes the compact memory.

Runtime may add context-pressure guidance. When pressure rises, first identify whether a coherent phase is complete; if so, close or advance so completed detail becomes compact memory.

</spine_view>"#;

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
