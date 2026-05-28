pub(crate) const SPINE_VIEW_INSTRUCTIONS: &str = r#"<spine_view>
Use Spine to organize long work into a task tree and keep context small. Each task node is a focused scope of work. The current node keeps its raw history visible.
When you close a node, runtime compacts that node's raw history into runtime-generated memory, returns to the parent node, and uses that memory instead of the full raw history in later turns.
Spine memory is internal context; never expose or imitate it in user-visible messages.

Spine tools are task-boundary controls:
- spine.tree: inspect the current Spine Tree, cursor, current live-node context pressure, and overall context-window pressure without moving it.
- spine.open(summary): start a focused child task under the current cursor. The summary is only a short tree label for the new child.
- spine.close(instruction?): finish the current non-root task node, compact its raw history into memory, and resume its parent. The optional instruction only guides what the compact memory should preserve.

Default to staying in the current live node while it remains focused. Use update_plan for ordinary task tracking; it does not create, close, compact, or move Spine nodes.
Spine context follows the current cursor: the current path remains visible, closed left siblings appear as memory, and the current live node keeps raw history.

Use Spine to model the evolving structure of the work. A good Spine tree makes the work readable: parents hold shared intent, constraints, decisions, and the phase map; siblings are peer phases at a similar level; children are focused subproblems whose result helps the parent continue.

For complex or long-running work, briefly sketch a provisional tree shape before starting: the parent goal, likely peer phases, and which phase begins now. Do not pre-open planned phases. Revisit and adjust the tree at coherent boundaries as the work changes.

Open a child when it gives a clear structural benefit:
- Dependency: the parent needs this focused result to continue cleanly.
- Compression: the work will produce noisy local reads, logs, experiments, tests, or reasoning that future work should see as memory rather than raw history.
- Focus: isolating the work helps avoid mixing phases or makes handoff cleaner.

When one phase reaches a coherent boundary and the next phase is a peer step in the same goal, close the current child, return to the parent, then open a sibling. Discovery, implementation, verification, documentation, and synthesis are usually siblings.

Close a node only at a coherent boundary, when it has enough motivation, judgment, evidence, and next-step context to return a compact result to the parent. Use the optional close instruction to name facts that must survive compaction. Do not close only because the turn is ending, context size crossed a soft threshold, or as end-of-response cleanup.

Context pressure is cumulative: even simple tasks can grow large after repeated user turns, tool outputs, and iterations. Manage local Spine boundaries before the session nears the context window; otherwise native/global compaction may open a new root epoch and broad raw history will leave the visible working context as coarse root memory. Prefer coherent task-local memories over relying on emergency global compaction.

When unsure, call spine.tree before moving Spine; use its node and global context-pressure stats as hints, not hard rules. When the current live node grows beyond about 50K node context, actively look for the next coherent close boundary, but never move Spine solely because a size threshold was crossed.

There is no production spine.next tool. Moving to a sibling phase is `close ; open`.

Do not open for routine continuation, another file read, another command, checklist updates, short replies, observations, ordinary turn boundaries, or one node per command, checklist item, or turn. Keep simple tasks in one node.
At root depth, close a root child to return to the root scope. Calling spine.close on the true root fails.
Runtime output may show `Base: <spine sidecar root>`; resolve sidecar-relative paths such as `nodes/.../memory.md` against that Base, not against the workspace cwd.
When moving between nodes, rely on the runtime Spine Tree and closed-node memories; completed Spine nodes are read-only, and sidecar trajs or memory files should be inspected only when historical details are genuinely needed.
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
