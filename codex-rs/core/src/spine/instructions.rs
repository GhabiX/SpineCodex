const SPINE_VIEW: &str = r#"<spine_view>
The Spine tree enables cost-efficient scaling by recursively decomposing tasks
into scoped nodes and merging them through compact continuation memory. Routine
bounded tasks remain lightweight, while difficult or open-ended tasks can scale
test-time compute toward the best attainable outcome. Just-in-time context
compilation turns each node's local working context into continuation memory.

Treat the Spine tree as the recursive semantic scope structure for task
decomposition and context compilation. Preserve its hierarchy carefully: every
transition must route work to the correct child, sibling, parent, or ancestor
scope.

Core workflow:

1. Begin a new top-level task with `open(<concrete, appropriately scoped task
   goal>)` once a root epoch is current.
2. At every node, maintain orientation to the overall task: the parent goal,
   the node's role, completed sibling work, any currently useful future plan,
   and the next action.
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
  most one Spine transition: `open`, `next`, or `close`. The transition sets
  the scope for the next ReAct step and does not affect the scope of other tool
  calls in the current step.
* Each transition is retained in the native rollout. `memory` is the
  model-authored continuation state that replaces the finalized node's local
  working content. Runtime preserves true user messages and closed child
  memories when assembling the complete memory, so do not copy them verbatim.
* Preserve `[U#]` anchors only when they are needed for correct continuation or
  traceability, such as unresolved user requests or the resolved referents of
  approvals, corrections, and elliptical replies. Do not maintain a separate
  request-status ledger when ordinary continuation state already captures the
  relevant intent.
* Root-epoch nodes cannot be closed, but child nodes may be opened from them.
* Spine transitions change task scope, not communication state. Report
  completed work directly to the user, avoid repeating results already
  communicated, and never create a reporting node or perform a transition
  solely for delivery.
</spine_view>"#;

const SPINE_VIEW_START_MARKER: &str = "\n\n<spine_view>";
const SPINE_TRIM_VIEW: &str = r#"<spine_trim_view>
Treat tool-response trimming as optional, conservative cleanup. Read and use
the latest completed tool result for the main task before deciding whether to
trim it. Only trim a tagged response when its original content is no longer
needed for correctness, debugging, citations, validation, or the user's
explanation. `spine.trim` applies only to a tagged response from the immediately
previous completed toolcall; a missed or expired `TRIM_ID` must not be retried.
Use `snip` to clear irrelevant content or `slice` to retain the needed head,
tail, or anchor window. Trimming changes only future visible context; raw
rollout evidence and toolcall structure remain intact.
</spine_trim_view>"#;

pub(crate) fn append(mut base: String, jit_enabled: bool, trim_enabled: bool) -> String {
    if !jit_enabled && !trim_enabled {
        return base;
    }
    if jit_enabled {
        if let Some(start) = base.rfind(SPINE_VIEW_START_MARKER) {
            base.truncate(start);
        }
    }
    if trim_enabled {
        if let Some(start) = base.rfind("\n\n<spine_trim_view>") {
            base.truncate(start);
        }
    }
    if jit_enabled || trim_enabled {
        if !base.is_empty() {
            base.push_str("\n\n");
        }
    }
    if jit_enabled {
        base.push_str(SPINE_VIEW);
    }
    if trim_enabled {
        if jit_enabled {
            base.push_str("\n\n");
        }
        base.push_str(SPINE_TRIM_VIEW);
    }
    base
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
        assert!(once.contains("<spine_trim_view>"));
        assert!(!once.contains("<spine_view>"));
        assert_eq!(append(once.clone(), false, true), once);
    }
}
