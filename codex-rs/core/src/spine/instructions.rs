pub(crate) const SPINE_VIEW_INSTRUCTIONS: &str = r#"<spine_view>
Use Spine as your task zipper and context manager. Completed scopes are folded into runtime-generated memory IR, and later turns carry the visible Spine Tree, completed memories, and the current live suffix instead of the full raw trace.
Spine Memory is internal context; never expose or imitate it in user-visible messages.
Use Spine effectively and efficiently.
Use update_plan only as the ordinary short-lived checklist for the current live work. It does not create, finish, close, compact, or move Spine nodes.
Default to staying in the current live node while it remains focused. Use update_plan to refresh the local checklist when evidence changes the immediate work.
Before starting a new coherent child scope, call spine.open, then immediately use update_plan in the new child if a local checklist is useful.
Move Spine at coherent scope boundaries rather than as a per-command habit:
- spine.open: start a focused child scope. It takes no arguments.
- spine.close: finish the current scope and return to its parent. Root cannot be closed. It requires `summary` for the closed scope.
To continue with a sibling, close the current child, then open the sibling from the resumed parent when that work actually begins.
spine.open/close are not end-of-response cleanup; when the current response still belongs to the current node, finish its user-visible work there, and only move Spine when entering or finishing a genuine scope.
Spine transitions are internal context-management steps, not substitutes for normal Codex turn delivery: after spine.close, continue work if the latest user request remains unfinished, or send the user-facing final answer/update if that request is complete, paused, blocked, or needs a decision. Do not use a Spine Tree update, tool output, or generated memory as the user-visible report.
Use spine.close to finish completed scopes after substantial raw history has accumulated or when future work is likely to reuse the generated memory IR.
At root depth, close a root child to return to the root scope. Calling spine.close on the root itself fails.
For spine.close, use summary as the short completion-time Spine Tree label for the current scope. Use the optional instruction argument when the automatic compact pass should prioritize specific facts to preserve from the completed scope. Do not use summary or instruction with spine.open.
Use spine.tree to inspect the current node and Spine Tree without moving the cursor.
Do not move spine only because a new user message arrived, because you answered a short question, or because you updated progress within the same scope.
Do not create one node per shell command, checklist item, short reply, or conversation turn.
After spine.close from `1.1.1` to `1.1`, the runtime folds `1.1.1`'s raw trace into `nodes/1/1/1/memory.md`; the parent `1.1` can continue with that child memory visible.
Runtime output may show `Base: <spine sidecar root>`; resolve sidecar-relative paths such as `nodes/.../memory.md` against that Base, not against the workspace cwd.
After spine.close, if unfinished work remains in the parent, use update_plan to refresh the current checklist from the generated memory, latest user intent, and current evidence.
Keep working in the current node while its raw details are still useful. When a coherent work scope is complete, fold it so later turns use its memory instead of its raw trace.
Avoid tiny splits for individual commands, small observations, or conversation turns.
The runtime may warn when the current node grows large: around 80k raw tokens, then every additional 30k. Treat the warning as a cue to finish the current scope cleanly, then use spine.close if the next work can rely on the generated memory IR.
When moving between nodes, rely on the runtime Spine Tree and generated memories; inspect sidecar trajs/memory files only when you need historical details.
Completed Spine nodes are read-only; rely on their memories instead of restating their old raw details.
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
