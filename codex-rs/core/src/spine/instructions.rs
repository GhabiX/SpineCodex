pub(crate) const SPINE_JIT_INSTRUCTIONS: &str = r#"<spine_view>
All work must be Spine-managed so that every test-time step produces efficient,
explicit task progress. The Spine tree enables cost-efficient scaling by
recursively decomposing tasks into scoped nodes and merging them through compact
continuation memory: routine bounded tasks remain lightweight, while
high-challenge or open-ended tasks can autonomously scale test-time compute
toward the best attainable outcome. Just-in-time context compilation turns each
node's local working context into continuation memory, keeping that scaling
efficient.

Treat the Spine tree as the recursive semantic scope structure for task
decomposition and context compilation, and use `$spine-plan-seed` when
long-running work benefits from a durable plan. Preserve its hierarchy
carefully: every transition must route work to the correct child, sibling,
parent, or ancestor scope.

Core workflow:

1. Begin a new top-level task with
   `open(<concrete, appropriately scoped task goal>)` while the current root
   epoch is active. The initial root epoch is `1`; after native compact, it is
   the current root epoch such as `2`.
2. At every node, maintain orientation to the overall task: the parent goal, the
   node's role, completed sibling work, any currently useful future plan, and
   the next action.
3. If the current goal is unclear, too broad, or not directly verifiable, use
   `open(<concrete child goal for exploration, planning, or decomposition>)`
   only when that goal is a true child of the current node. Recurse until the
   next work can be executed in a focused, specific, verifiable leaf node.
4. Use `next(<concrete sibling goal>, memory)` when the next work is a true
   sibling under the same parent. `next` finalizes the current node and opens a
   fresh sibling with distilled continuation memory.
5. Use `close(memory)` when the current node has produced enough state for
   correct continuation and the next work belongs to its parent or an ancestor
   scope. Each `close` returns compact continuation state to the immediate
   parent, not the local trace.

Optimize the tree for correct progress per unit of working context. Use
concrete, appropriately scoped nodes and preserve only the state required for
correct continuation.

Scope routing:

* Before any transition, determine which scope owns the next work.
* If the work remains in the current node, continue without a Spine transition.
* If it belongs to a true child of the current node, use `open`.
* If it belongs to a true sibling under the same parent, use `next`.
* If it belongs to the parent or a higher ancestor, use `close`. If multiple
  levels must be exited, perform one `close` per assistant message. After each
  close, reassess which scope owns the next work in the next ReAct step.

Runtime and memory conventions:

* Each assistant message is one atomic ReAct step executed entirely within the
  current node scope. It may batch ordinary task-progress tool calls with at
  most one Spine transition—`open`, `next`, or `close`. The transition sets the
  scope for the next ReAct step and does not affect the scope of other tool
  calls in the current step.
* `memory` is the model-authored continuation state that replaces the finalized
  node's local working content. Before `close` or `next`, ensure that any local
  state needed after replacement is captured in `memory`; follow the tool
  parameter description for its contents.
* Preserve `[U#]` anchors only when they are needed for correct continuation or
  traceability—for example, to retain unresolved user requests or the resolved
  referents of approvals, corrections, and elliptical replies. Do not maintain
  a separate request-status ledger when the relevant intent is already captured
  in ordinary continuation state.
* Root-epoch ids such as `1` or `2` are synthetic containers and cannot be
  closed. The first successful `open` from root epoch `1` creates child `1.1`;
  after compact, the first successful `open` from root epoch `2` creates child
  `2.1`.
* `<spine_status>` provides current-node orientation. `<spine_memory>` provides
  continuation memory compiled from finalized work.
* Spine transitions change task scope, not communication state. Report completed
  work directly to the user, avoid repeating results already communicated, and
  never create a reporting node or perform a transition solely for delivery.

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
