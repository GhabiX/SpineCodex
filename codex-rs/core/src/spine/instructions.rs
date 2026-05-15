pub(crate) const SPINE_VIEW_INSTRUCTIONS: &str = r#"<spine_view>
Use Spine as your task plan and context manager. Completed scopes are folded into runtime-generated worklog IR, and later turns carry the visible Spine Tree, completed worklogs, and the current live suffix instead of every old raw message.
Use Spine effectively and efficiently.
At the start, use update_plan with spine_plantree to maintain one compact task tree draft for the current editable scope. This is planning only; it does not create Spine nodes or move the cursor.
Use update_plan's top-level plan for the current real Spine node's checklist; use each future child scope's checkpoints inside spine_plantree.root.children for that future scope's checklist.
Future PlanTree scopes may display as `~<predicted-id>` to distinguish planned nodes from real Spine nodes.
Default to staying in the current live node while it remains focused. Use update_plan to revise the current PlanTree when new evidence changes the task structure.
When your task structure or next work scope changes, promptly refresh the current spine_plantree with update_plan so the displayed PlanTree stays current.
For non-trivial or multi-phase work, keep future planned scopes in `spine_plantree.children` rather than flattening them into root checkpoints, and update them with `update_plan`; this manages planning only and does not create real Spine nodes.
Move Spine when a completed scope has accumulated substantial raw history and future work is likely to reuse its generated worklog IR:
- spine.open: enter a focused child scope that should inherit the parent goal but keep its own local context; it takes no arguments.
- spine.next: finish the current leaf and move to its next sibling.
- spine.close: finish the current leaf, close its non-root parent scope, and continue at the parent's next sibling. Root cannot be closed.
At root depth, use spine.next to finish the current root child and continue with its next sibling; use spine.close only from a nested scope when closing its parent and returning to the parent's next sibling.
For spine.next or spine.close, use summary as the short completion-time Spine Tree label. Use the optional instruction argument when the automatic compact pass should prioritize specific facts to preserve from the completed leaf or scope. Do not use summary or instruction with spine.open.
Use spine.tree to inspect the current node and Spine Tree without moving the cursor.
Do not move spine only because a new user message arrived, because you answered a short question, or because you updated progress within the same scope.
Do not create one node per shell command, checklist item, short reply, or conversation turn.
After spine.next from `1.1` to `1.2`, the runtime folds `1.1`'s raw trace into `nodes/1/1/worklog.md`; later context shows the Spine Tree plus `1.1` worklog, not `1.1` raw trace.
After spine.close from `1.1.2` to `1.2`, the runtime folds the completed `1.1` scope into `nodes/1/1/worklog.md`; child scopes that were already folded are carried through the Spine Tree/worklog IR, while raw child traces stay expandable out of band.
Runtime output may show `Base: <spine sidecar root>`; resolve sidecar-relative paths such as `nodes/.../worklog.md` against that Base, not against the workspace cwd.
After spine.next or spine.close, if unfinished work remains, use update_plan to refresh the current PlanTree from the handoff summary and current evidence.
Keep working in the current node while its raw details are still useful. When a coherent work scope is complete, fold it so later turns use its worklog instead of its raw trace.
Avoid tiny splits for individual commands, small observations, or conversation turns.
The runtime may warn when the current node grows large: around 50k raw tokens, then every additional 30k. Treat the warning as a cue to finish the current scope cleanly, then use spine.next or spine.close if the next work can rely on the worklog.
When moving between nodes, rely on the runtime Spine Tree and generated worklogs; inspect sidecar trajs/worklog files only when you need historical details.
Completed Spine nodes are read-only; rely on their worklogs instead of restating their old PlanTree checkpoints.
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
