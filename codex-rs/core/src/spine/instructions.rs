pub(crate) const SPINE_VIEW_INSTRUCTIONS: &str = r#"<spine_view>
Use spine as your task plan and context manager for long-running work, not as a per-message or per-turn log. A node is the active working context: keep one coherent goal, evidence set, decisions, and plan inside it.
At the start, form a compact Spine plan: one node for simple tasks, or a small tree of focused phase-level scopes when context can later be carried by summary/worklog.
Default to staying in the current live node while it remains focused. Use update_plan as the checklist inside the current active scope for local steps, verification items, and short-lived task tracking.
Move spine only when a scope boundary improves focus, cost, or future recall:
- spine.open: enter a child scope for a focused subproblem that should inherit the parent goal but keep its own local context.
- spine.next: finish the current leaf and move to its next sibling when the current phase has a clear handoff, or when accumulated local context has become noisy enough that the next phase should start clean.
- spine.close: finish the current leaf and close its non-root parent scope, then continue at the parent's next sibling. Root cannot be closed.
Use spine.tree to inspect the current node and Spine Tree without moving the cursor.
Do not move spine only because a new user message arrived, because you answered a short question, or because you updated progress within the same scope.
Good boundaries look like `investigate/localize -> implement/verify`; bad boundaries look like one node per shell command, one node per checklist item, or one node per conversation turn.
Each spine summary should describe the scope handoff: what was learned, decided, verified, or intentionally isolated.
Prefer the smallest tree that keeps the active reasoning context clean; avoid both one-node context bloat and one-turn-per-node fragmentation.
When moving between nodes, rely on the runtime Spine Tree and generated worklogs; inspect sidecar files only when you need historical details.
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
