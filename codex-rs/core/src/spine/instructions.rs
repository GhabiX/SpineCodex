pub(crate) const SPINE_VIEW_INSTRUCTIONS: &str = r#"<spine_view>
Spine is the primary framework for managing task structure, attention focus, and context for every task. At the start of each request, use the current Spine tree to decide whether to stay in the current scope, open a focused child, advance to a sibling phase, or close completed work. Keeping Spine healthy is part of doing the task, not optional bookkeeping. Move Spine only when the work reaches a meaningful scope boundary.

Mental model:
- Current node = live scratchpad for one coherent phase.
- Closed node = runtime-generated compact memory for future orientation.
- spine.open = enter a narrower blocker or dependency.
- spine.next = finish this phase and start a sibling phase.
- spine.close = finish this scope and return to its parent.

Context savings happen only when a live node is closed: `spine.close` and the close step of `spine.next` replace that node's raw history with compact memory in future prompts. `spine.open` only creates a child boundary for narrower work; it does not shrink the current context window by itself.

Tools:
- spine.tree: inspect the tree, cursor, and context pressure without moving the cursor.
- spine.open(summary): open a focused child under the current node.
- spine.close(instruction?): close the current node, compact its raw history into memory, and resume the parent.
- spine.next(summary, instruction?): close the current node, preserve compact guidance as memory, then continue in a new sibling under the resumed parent.

At the start of a task, assume its shape may grow as evidence appears. Orient from the current tree, keep the tree ready to absorb new phases, and let structure emerge at natural boundaries. After every spine.open/close/next result, use the returned tree as the source of truth for cursor, parent, siblings, and the next boundary.

Scope discipline: keep umbrella/parent nodes for coordination, not as long work buffers. Before substantive research, implementation, verification, or long task-specific discussion, open a focused child; keep only routing, approvals, cross-phase decisions, and final synthesis in the parent.

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

For spine.close and spine.next, keep the optional instruction to one short sentence about what future work should preserve. Do not write a full compact summary in the tool argument; runtime owns compact memory generation.

Runtime may add context-pressure guidance: a boundary hint when the current live node reaches about 50K context tokens and each later 25K band, and a context warning when the overall prompt reaches about 80% of the model window. Under pressure, use `spine.open` only for a genuine narrower blocker; to reduce context, close or advance a completed scope so runtime can compact it.

Use update_plan only as a checklist. In Plan mode, inspect with `spine.tree` only.
</spine_view>"#;

pub(crate) fn append_spine_view_instructions(
    mut base_instructions: String,
    enabled: bool,
) -> String {
    if !enabled || base_instructions.contains(SPINE_VIEW_INSTRUCTIONS) {
        return base_instructions;
    }

    if !base_instructions.is_empty() {
        base_instructions.push_str("\n\n");
    }
    base_instructions.push_str(SPINE_VIEW_INSTRUCTIONS);
    base_instructions
}

#[cfg(test)]
#[path = "instructions_tests.rs"]
mod tests;
