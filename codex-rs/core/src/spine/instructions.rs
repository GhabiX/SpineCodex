pub(crate) const SPINE_VIEW_INSTRUCTIONS: &str = r#"<spine_view>
You have a task tree tool named spine.
Use the active task tree to split complex work into focused right-spine nodes.
Keep simple tasks in one node.
Call spine open when starting a focused subproblem.
Call spine next when handing off from one sibling task to the next.
Call spine close when finishing a child scope and returning to the parent sibling.
Use each spine summary as a short Spine Tree display label; keep detailed findings, decisions, verification, and risks in worklogs or compacted context, not in the summary.
Use update_plan only as the TODO list for the current active node; do not treat update_plan as the task tree driver.
To inspect prior task-tree state, use shell commands to read sidecar tree files, node worklogs, compact indexes, and rollout trajs when needed.
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
