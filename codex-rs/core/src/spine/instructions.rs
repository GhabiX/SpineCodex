use std::path::Path;

pub(crate) const SPINE_VIEW_INSTRUCTIONS: &str = r#"<spine_view>
Spine is the core control frame for organizing work as a task tree while completing any task. A Spine node is a natural task boundary around a coherent work objective. Context, attention, compact memory, and token cost are managed through those task boundaries. At the start of each substantive request, place the work under Spine control by opening a focused task boundary when the current node is an umbrella/root scope or otherwise too broad; if the current node is already a suitable focused boundary, continue there. Treat new requests as potentially complex until the work proves otherwise, and move Spine only when the work reaches a real boundary such as a narrower subproblem, peer shift, completed scope, or material user redirect so task-specific context does not accumulate in a parent node.

Mental model:
- Current node = the active task boundary around the coherent work objective currently being pursued.
- Closed node = runtime-generated compact memory from a completed task boundary.
- spine.open = enter a focused child for a substantial blocker or dependency.
- spine.next = finish this task boundary and start a sibling boundary.
- spine.close = finish this scope and return to its parent.

Context savings happen only when a live node is closed: `spine.close` and the close step of `spine.next` replace that node's raw history with compact memory in future prompts. `spine.open` only creates a child boundary for narrower work; it does not shrink the current context window by itself.

Treat `<spine_status .../>`, pressure hints, and Spine tool outputs as temporary orientation for the current cursor and boundary choice. Treat `<spine_memory ...>` as compact memory from previously closed work for orientation, not as a new user request; task summaries should preserve task facts, decisions, evidence, risks, and resume focus.

Tools:
- spine.tree: inspect the tree, cursor, and context pressure when that state is unclear.
- spine.open(summary): open a focused child under the current node when a narrower boundary helps the parent objective. Write summary as a short motivation/boundary label.
- spine.close(instruction?): close the current node, compact its raw history into memory, and resume the parent.
- spine.next(summary, instruction?): close the current node, preserve compact guidance as memory, then continue in a new sibling under the resumed parent. Write summary with the same rule as spine.open: label why the next sibling phase exists now, not a recap of the node being closed.

Spine summaries are user-facing labels; write them in the user's current language when reasonably clear.

Let task structure emerge from coherent work objectives and natural task boundaries. After spine.open/close/next, use the returned Spine projection as the source of truth for cursor, parent, and siblings.

Scope discipline: keep umbrella/parent nodes for coordination, not as long work buffers. For substantive work, make sure task-specific context lives in a focused task boundary when the current node is root, umbrella, stale, or too broad. After that, let the shape of the work decide: continue in the current node while the same objective holds, and move Spine only for a real narrower subproblem, peer shift, completed scope, or material user redirect. Do not split Spine for individual commands, checklist items, tiny updates, or fixed process templates.

Move Spine by matching the next work to the tree:
- Same objective or boundary: continue in the current node.
- Narrower blocker or dependency: open a child whose result helps the parent continue.
- Peer objective or phase: use spine.next to close this boundary and continue as a sibling.
- Finished boundary: close with one sentence naming what later work should remember.
- Material user redirect: close or advance the stale phase before continuing.
- Settled facts are being repeated or re-audited: close or advance first.

Good Spine boundaries follow coherent work objectives rather than a fixed process template: use sibling phases for substantial peer shifts in focus, use focused children for narrower investigations or blockers, and close completed scopes when their details should become compact memory. As context grows, prefer the next coherent handoff so completed detail becomes memory before the live node gets too large.

Examples:
- If a broad request starts at root, open one focused task boundary before deep inspection.
- If a new subproblem appears inside focused work, use spine.open for that narrower blocker.
- If the current focus changes to a peer concern or the user redirects, use spine.next or close/advance.

For spine.close and spine.next, pass only one short compact-guidance sentence about what future work should preserve; runtime writes the compact memory.

Runtime may add context-pressure guidance. When pressure rises, first identify whether a coherent task boundary is complete; if so, close or advance so completed detail becomes compact memory.

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
