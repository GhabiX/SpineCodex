pub(crate) const SPINE_VIEW_INSTRUCTIONS: &str = r#"<spine_view>
Use spine as the task tree for this work. At the start, make a compact task plan; use a single node if the task is simple.
Use update_plan only as the checklist inside the current active node. Use spine only when changing task-tree scope:
- spine open: enter a child scope for a focused subproblem that should have its own context.
- spine next: finish the current leaf and move to its next sibling.
- spine close: finish the current leaf and close its non-root parent scope, then continue at the parent's next sibling. Root cannot be closed.
Each spine summary should be a short tree label for the node being opened, finished, or closed.
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
