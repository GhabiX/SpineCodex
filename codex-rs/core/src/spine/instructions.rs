pub(crate) const SPINE_JIT_INSTRUCTIONS: &str = r#"<spine_view>
All work must be Spine-managed. The Spine tree enables cost-efficient scaling
by recursively decomposing tasks into scoped nodes and merging finalized work
through compact continuation memory: routine bounded tasks remain lightweight,
while difficult or open-ended tasks can autonomously scale test-time compute
toward the best attainable outcome. Just-in-time context compilation keeps this
scaling efficient by turning each node's local working context into continuation
memory.

Treat the Spine tree as the task's semantic scope hierarchy: each piece of work
belongs in the node that owns it, and every transition must follow its child,
sibling, parent, or ancestor relationship. Use `$spine-plan-seed` when
long-running work benefits from a durable plan.

Core workflow:

1. Begin a new top-level task with
   `open(<concrete, appropriately scoped task goal>)` while the current root
   epoch is active.
2. Maintain orientation at every node to the parent goal, the node's role,
   relevant completed sibling work, any useful future plan, and the next action.
3. If the current goal is unclear, too broad, or not directly verifiable, use
   `open(<concrete child goal for exploration, planning, or decomposition>)`
   only when that goal is a true child of the current node. Recurse until the
   next work belongs in a focused, specific, and verifiable leaf node.
4. Use `next(<concrete sibling goal>, memory)` when the next work is a true
   sibling under the same parent.
5. Use `close(memory)` when the current node has produced enough state for
   correct continuation and the next work belongs to its parent or an ancestor
   scope. Each `close` returns to the immediate parent.

Conventions:

* Use at most one Spine transition per assistant turn. Ordinary task tools may
  accompany it and belong to the resulting node; the transition applies to the
  current node's prior ReAct history.
* After `close` or `next`, `memory` replaces the finalized node's local working
  content; follow the tool parameter description to preserve the state required
  for continuation. Runtime preserves user messages and child memories, so use
  Node Memory for the additional continuation state they do not already
  provide.
* Root epochs are synthetic containers and cannot be closed.
* `<spine_status>` provides current-node orientation. `<spine_memory>` provides
  continuation memory compiled from finalized work.
* Spine nodes define task-semantic boundaries rather than user-response
  boundaries, so answer the user as soon as useful and create nodes only for
  work that needs its own task scope.

</spine_view>
"#;

const SPINE_VIEW_START_MARKER: &str = "\n\n<spine_view>";
// The Trim segment is intentionally empty until its model-visible copy is approved.
const SPINE_TRIM_INSTRUCTIONS: &str = "";

pub(crate) fn append(
    mut base_instructions: String,
    spine_jit_enabled: bool,
    spine_trim_enabled: bool,
) -> String {
    let trim_segment = spine_trim_enabled.then_some(SPINE_TRIM_INSTRUCTIONS);
    if !spine_jit_enabled && trim_segment.map_or(true, str::is_empty) {
        return base_instructions;
    }

    let jit_segment = if spine_jit_enabled {
        if let Some(start) = base_instructions.rfind(SPINE_VIEW_START_MARKER) {
            base_instructions.truncate(start);
        }
        Some(SPINE_JIT_INSTRUCTIONS)
    } else {
        None
    };

    for instructions in [jit_segment, trim_segment].into_iter().flatten() {
        if instructions.is_empty() || base_instructions.contains(instructions) {
            continue;
        }
        if !base_instructions.is_empty() {
            base_instructions.push_str("\n\n");
        }
        base_instructions.push_str(instructions);
    }
    base_instructions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_off_is_identity() {
        let base = "base instructions".to_string();
        assert_eq!(append(base.clone(), false, false), base);
    }

    #[test]
    fn enabled_instructions_are_idempotent() {
        let once = append("base".to_string(), true, false);
        assert_eq!(append(once.clone(), true, false), once);
    }

    #[test]
    fn enabled_instructions_replace_an_existing_spine_segment() {
        let replaced = append(
            "base\n\n<spine_view>old instructions</spine_view>".to_string(),
            true,
            false,
        );
        assert!(!replaced.contains("old instructions"));
        assert_eq!(replaced.matches("<spine_view>").count(), 1);
    }

    #[test]
    fn trim_instructions_are_independent_and_idempotent() {
        let once = append("base".to_string(), false, true);
        assert_eq!(once, "base");
        assert_eq!(append(once.clone(), false, true), once);
    }
}
