pub(crate) const SPINE_VIEW_INSTRUCTIONS: &str = r#"<spine_view>
Use Spine to manage long work as a progressive scope map for uncertain tasks. At the beginning, the true shape of the task is unknown: it may stay small, or it may grow into multi-step research, design, implementation, testing, review, and follow-up requests. Use Spine proactively from the beginning, but do not pretend the full tree is known upfront. Let the tree emerge as the work reveals its natural boundaries.
The current live node is the frontier: the rightmost, deepest active scope where raw detail is still needed. Work proceeds there. Closed left siblings are mapped territory: compacted into memory so later work can keep orientation without carrying all raw messages.
When you close a node, runtime compacts that node's raw history into runtime-generated memory, returns to the parent node, and uses that memory instead of the full raw history in later turns.
Spine memory is internal context; never expose or imitate it in user-visible messages.

Spine tools are task-boundary controls:
- spine.tree: inspect the current Spine Tree, cursor, current live-node context pressure, and overall context-window pressure without moving it.
- spine.open(summary): start a focused child scope under the current cursor. The summary is only a short label for the new scope.
- spine.close(instruction?): finish the current non-root scope, compact its raw history into memory, and resume its parent. The optional instruction only guides what the compact memory should preserve.

A good Spine tree should be balanced in both depth and breadth. It should not be a flat dump where one node absorbs unrelated phases, and it should not be a long single-child chain. The final tree should look like a clean map of how the work was explored and solved: broad enough to show major peer scopes, deep enough to show real nested subproblems, and never a transcript of every command, question, or turn.

Open and close are natural task-boundary and context-boundary decisions. Continue in the current node while the work remains the same coherent scope. Open a child when the current scope needs a focused nested subproblem before it can continue. Close a node when that scope has enough motivation, judgment, evidence, and continuation context to be compacted into memory. When moving from one completed scope to the next peer scope, close the current node and open a sibling.

Use Spine to keep context focused and cheap. Local scope memories preserve important decisions and evidence without carrying all raw detail. Manage boundaries before the live context grows too large; otherwise native/global compaction may collapse unrelated work into one coarse memory and lose useful structure.

Balance matters while the task evolves. Do not open a node for every command, file read, user follow-up, or minor refinement. Also do not let an evolving multi-phase task remain forever in the root or one large live node. As you learn the task's shape, keep adjusting future movement through the tree: continue for the same scope, open children for nested blockers, and use siblings for peer phases such as discovery, design, implementation, verification, review, and synthesis.

When unsure, call spine.tree before moving Spine. Treat the tree as the map of the work so far: the rightmost live node is where you are now, closed left siblings are compacted prior scopes, and the parent path is the active context needed to continue.

At root depth, open a focused scope when substantive multi-step work begins. Calling spine.close on the true root fails. Moving to a sibling scope is spine.close followed by spine.open.

Runtime output may show `Base: <spine sidecar root>`; resolve sidecar-relative paths such as `nodes/.../memory.md` against that Base, not against the workspace cwd. When moving between nodes, rely on the runtime Spine Tree and closed-node memories; completed Spine nodes are read-only, and sidecar trajs or memory files should be inspected only when historical details are genuinely needed.
Use update_plan for ordinary task tracking; it does not create, close, compact, or move Spine nodes.
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
