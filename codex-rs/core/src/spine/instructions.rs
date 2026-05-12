pub(crate) const SPINE_VIEW_INSTRUCTIONS: &str = r#"<spine_view>
Use Spine as your task plan and context manager. Completed scopes are folded into runtime-generated worklog IR, and later turns carry the visible Spine Tree, completed worklogs, and the current live suffix instead of every old raw message.
Use Spine effectively and efficiently.
At the start, form a compact Spine plan: one node for simple tasks, or a small tree of focused scopes for longer work. Revise the tree when new evidence changes the task structure.
Default to staying in the current live node while it remains focused. Use update_plan as the checklist inside the current active scope for local steps, verification items, and short-lived task tracking.
Move Spine when a completed scope has accumulated substantial raw history and future work is likely to reuse its generated worklog IR:
- spine.open: enter a focused child scope that should inherit the parent goal but keep its own local context.
- spine.next: finish the current leaf and move to its next sibling.
- spine.close: finish the current leaf, close its non-root parent scope, and continue at the parent's next sibling. Root cannot be closed.
At root depth, use spine.next to finish a phase and continue with the next root sibling; use spine.close only from a nested scope when closing its parent and returning to the parent's next sibling.
For spine.next or spine.close, use the optional instruction argument when the automatic compact pass should prioritize specific facts to preserve from the completed leaf or scope; keep summary as the short Spine Tree label, and do not use instruction with spine.open.
Use spine.tree to inspect the current node and Spine Tree without moving the cursor.
Do not move spine only because a new user message arrived, because you answered a short question, or because you updated progress within the same scope.
Do not create one node per shell command, checklist item, short reply, or conversation turn.
After spine.next from `1.1` to `1.2`, the runtime folds `1.1`'s raw trace into `nodes/1/1/worklog.md`; later context shows the Spine Tree plus `1.1` worklog, not `1.1` raw trace.
After spine.close from `1.1.2` to `1.2`, the runtime folds the completed `1.1` scope into `nodes/1/1/worklog.md`; child scopes that were already folded are carried through the Spine Tree/worklog IR, while raw child traces stay expandable out of band.
Runtime output may show `Base: <spine sidecar root>`; resolve sidecar-relative paths such as `nodes/.../worklog.md` against that Base, not against the workspace cwd.
After spine.next or spine.close, if unfinished work remains, immediately call update_plan in the new current node to rebuild the checklist from the handoff summary and current evidence; the runtime does not carry old checklist items forward.
Keep working in the current node while its raw details are still useful. When a coherent work scope is complete, fold it so later turns use its worklog instead of its raw trace.
Avoid tiny splits for individual commands, small observations, or conversation turns.
The runtime may hint when the current node grows large: around 60k raw tokens, then every additional 30k. Treat the hint as a cue to finish the current scope cleanly, then use spine.next or spine.close if the next work can rely on the worklog.
When moving between nodes, rely on the runtime Spine Tree and generated worklogs; inspect sidecar trajs/worklog files only when you need historical details.
In Plan mode, do not call mutating spine operations.
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

pub(crate) fn strip_spine_view_instructions(base_instructions: &str) -> &str {
    let Some(base_without_spine_view) = base_instructions.strip_suffix(SPINE_VIEW_INSTRUCTIONS)
    else {
        return base_instructions;
    };

    base_without_spine_view
        .strip_suffix("\n\n")
        .unwrap_or(base_without_spine_view)
}

#[cfg(test)]
#[path = "instructions_tests.rs"]
mod tests;
