pub(crate) const SPINE_VIEW_INSTRUCTIONS: &str = r#"<spine_view>
Use Spine as your task plan and context manager. Completed scopes are folded into runtime-generated memory IR, and later turns carry the visible Spine Tree, completed memories, and the current live suffix instead of every old raw message.
Spine Memory is internal context; never expose or imitate it in user-visible messages.
Use Spine effectively and efficiently.
Use update_plan with task_projection as the single Spine planning input. One call should include both task_projection.current.checklist (the current real Spine node checklist) and task_projection.draft_nodes (future planned scopes, each with summary/checklist and a parent real node id or earlier ~draft_id for nesting).
task_projection is planning only: it does not create, finish, close, compact, or move Spine nodes. Do not combine task_projection with top-level plan, and never send spine_plantree as input. Successful writable updates return spine_tree; treat it as authoritative. A returned normalized spine_plantree is runtime output only.
Default to staying in the current live node while it remains focused. Use update_plan to refresh task_projection when evidence changes the task structure.
Before starting a new coherent work scope, compare it with current planned draft children: if it matches one, call spine.open to materialize it before doing the work, then immediately call update_plan in the new child using that draft's summary/checklist. If it no longer matches, update task_projection first; do not bypass planned child scopes with spine.next from the parent.
Move Spine at coherent scope boundaries rather than as a per-command habit:
- spine.open: start a focused child scope that should inherit the parent goal but keep its own local context; use it before working on a matching planned child scope. It takes no arguments.
- spine.next: finish the current leaf and move to its next sibling when the next work is sibling-level under the same parent.
- spine.close: finish the current leaf, close its non-root parent scope, and continue at the parent's next sibling when the parent scope is complete. Root cannot be closed. It requires `child_summary` for the current leaf and `summary` for the parent scope.
spine.next/close are not end-of-response cleanup; when the current response still belongs to the current node, finish its user-visible work there, and only move Spine when beginning genuinely new sibling/parent-sibling work.
Spine transitions are internal context-management steps, not substitutes for normal Codex turn delivery: after spine.next or spine.close, continue work if the latest user request remains unfinished, or send the user-facing final answer/update if that request is complete, paused, blocked, or needs a decision. Do not use a Spine Tree update, tool output, or generated memory as the user-visible report.
Use spine.next or spine.close to fold completed scopes after substantial raw history has accumulated or when future work is likely to reuse the generated memory IR.
At root depth, use spine.next to finish the current root child and continue with its next sibling; use spine.close only from a nested scope when closing its parent and returning to the parent's next sibling.
For spine.next, use summary as the short completion-time Spine Tree label. For spine.close, use child_summary as the label for the current leaf and summary as the label for the parent scope. Use the optional instruction argument when the automatic compact pass should prioritize specific facts to preserve from the completed leaf or scope. Do not use summary, child_summary, or instruction with spine.open.
Use spine.tree to inspect the current node and Spine Tree without moving the cursor.
Do not move spine only because a new user message arrived, because you answered a short question, or because you updated progress within the same scope.
Do not create one node per shell command, checklist item, short reply, or conversation turn.
After spine.next from `1.1` to `1.2`, the runtime folds `1.1`'s raw trace into `nodes/1/1/memory.md`; later context shows the Spine Tree plus `1.1` memory, not `1.1` raw trace.
After spine.close from `1.1.2` to `1.2`, the runtime first folds the closing child into `nodes/1/1/2/memory.md`, then folds the completed `1.1` scope into `nodes/1/1/memory.md`; child scopes remain available as durable memory IR while parent context uses the parent memory by default.
Runtime output may show `Base: <spine sidecar root>`; resolve sidecar-relative paths such as `nodes/.../memory.md` against that Base, not against the workspace cwd.
After spine.next or spine.close, if unfinished work remains, use update_plan to refresh the current PlanTree from the generated memory, latest user intent, and current evidence.
Keep working in the current node while its raw details are still useful. When a coherent work scope is complete, fold it so later turns use its memory instead of its raw trace.
Avoid tiny splits for individual commands, small observations, or conversation turns.
The runtime may warn when the current node grows large: around 80k raw tokens, then every additional 30k. Treat the warning as a cue to finish the current scope cleanly, then use spine.next or spine.close if the next work can rely on the memory.
When moving between nodes, rely on the runtime Spine Tree and generated memories; inspect sidecar trajs/memory files only when you need historical details.
Completed Spine nodes are read-only; rely on their memories instead of restating their old PlanTree checkpoints.
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
