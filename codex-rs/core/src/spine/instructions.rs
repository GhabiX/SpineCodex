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

Use Spine to model the evolving structure of the work as a balanced task tree. A good Spine tree reflects the real decomposition of the work: parents hold shared intent, constraints, decisions, and the phase map; children are coherent peer phases or independently compactable subproblems; each child may itself have a subtree when its own work has real nested structure.

For complex or long-running work, briefly sketch a provisional tree shape before starting: the parent goal, likely peer phases, and which phase begins now. Do not pre-open planned phases. Revisit and adjust the tree at coherent boundaries as the work changes. For example, a review task may have sibling phases for discovery, focused subsystem review, test review, and synthesis; any one of those phases may open children only when it has focused nested dependencies of its own.

Before changing Spine, classify the next work:
- Continuation: same scope as the current live node. Keep working in the current node.
- Child: a nested dependency or focused subproblem whose result the current scope needs before it can continue. Open a child.
- Peer: a next phase at the same abstraction level under the same parent. Close the current child, return to the parent, then open a sibling.
- Parent-level redirect: the user shifts back to the parent goal or asks to re-evaluate the current structure. Close the current child before continuing at the parent level when the child has enough context to compact.

Open a child when it gives a clear structural benefit:
- Decomposition: the child is a real refinement of the current scope, not a duplicate label for the same work.
- Dependency: the parent needs this focused result to continue cleanly.
- Compression: the work will produce noisy local reads, logs, experiments, tests, or reasoning that future work should see as memory rather than raw history.
- Focus: isolating the work helps avoid mixing phases or makes handoff cleaner.

When one phase reaches a coherent boundary and the next phase is a peer step in the same goal, close the current child, return to the parent, then open a sibling. Discovery, implementation, verification, documentation, review, and synthesis are usually siblings when they are phases of the same parent goal. Do not open a peer phase as a child of the previous peer phase.

Close a node only at a coherent boundary, when it has enough motivation, judgment, evidence, and next-step context to return a compact result to the parent. Use the optional close instruction to name facts that must survive compaction. Do not close only because the turn is ending, context size crossed a soft threshold, or as end-of-response cleanup.

Context pressure is cumulative: even simple tasks can grow large after repeated user turns, tool outputs, and iterations. Manage local Spine boundaries before the session nears the context window; otherwise native/global compaction may open a new root epoch and broad raw history will leave the visible working context as coarse root memory. Prefer coherent task-local memories over relying on emergency global compaction.

When unsure, call spine.tree before moving Spine; use its node and global context-pressure stats as hints, not hard rules. When the current live node grows beyond about 50K node context, actively look for the next coherent close boundary, but never move Spine solely because a size threshold was crossed.

Moving to a sibling phase is `spine.close` followed by `spine.open`.

Do not open for routine continuation, another file read, another command, checklist updates, short replies, observations, ordinary turn boundaries, or one node per command, checklist item, or turn. Keep simple tasks in one node.
Control both depth and breadth. Repeated or paraphrased summaries along the current path are a sign of accidental over-nesting; continue in place or close back to the right parent instead of opening another child. A long chain of single-child nodes usually means peer phases or continuations were modeled as children. A wide list of tiny nodes usually means commands or checklist items were modeled as phases. Prefer a readable subtree with meaningful phases and focused subproblems.
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
