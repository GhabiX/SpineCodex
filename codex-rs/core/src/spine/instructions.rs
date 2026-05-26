pub(crate) const SPINE_VIEW_INSTRUCTIONS: &str = r#"<spine_view>
Use Spine to organize long work into a task tree and keep context small. Each task node is a focused scope of work. The current node keeps its raw history visible.
When you close a node, runtime compacts that node's raw history into runtime-generated memory, returns to the parent node, and uses that memory instead of the full raw history in later turns.
Spine memory is internal context; never expose or imitate it in user-visible messages.

Spine tools are task-boundary controls:
- spine.tree: inspect the current Spine Tree and cursor without moving it.
- spine.open(summary): start a focused child task under the current cursor. The summary is only a short tree label for the new child.
- spine.close(instruction?): finish the current non-root task node, compact its raw history into memory, and resume its parent. The optional instruction only guides what the compact memory should preserve.

Default to staying in the current live node while it remains focused. Use update_plan as the ordinary short-lived checklist for the current live work; it does not create, finish, close, compact, or move Spine nodes.
Move Spine only at coherent task boundaries. Do not call spine.open because you are continuing the same investigation, reading another file, running another command, updating a checklist, answering a short question, or starting a new conversation turn.
Do not create one node per shell command, checklist item, short reply, observation, or turn. Keep simple tasks in one node.
Before starting a genuine nested subproblem that needs its own compactable context, call spine.open(summary), then use update_plan inside that child if a local checklist is useful.
When the nested task is complete, call spine.close(instruction?) to return to the parent. After close, continue parent work if the latest user request remains unfinished, or send the user-facing response if the request is complete, paused, blocked, or needs a decision.
To continue with a sibling task, first close the current child. Open the sibling from the resumed parent only when that sibling work actually begins.
There is no production spine.next tool. Treat next-like movement as two explicit steps: close the current child, then open a new sibling from the resumed parent.

Use spine.close after substantial raw history has accumulated or when future work can rely on the runtime-generated memory. Do not treat spine.open/close as end-of-response cleanup.
At root depth, close a root child to return to the root scope. Calling spine.close on the true root fails.
Runtime output may show `Base: <spine sidecar root>`; resolve sidecar-relative paths such as `nodes/.../memory.md` against that Base, not against the workspace cwd.
When moving between nodes, rely on the runtime Spine Tree and closed-node memories; inspect sidecar trajs or memory files only when historical details are needed.
Completed Spine nodes are read-only; rely on their memories instead of rewriting old nodes.
In Plan mode, do not call mutating Spine operations.
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
