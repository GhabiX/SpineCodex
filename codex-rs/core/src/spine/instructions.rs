use std::path::Path;

pub(crate) const SPINE_JIT_INSTRUCTIONS: &str = r#"<spine_view>
All work must be Spine-managed to make every test-time step produce efficient,
explicit task progress: the Spine tree enables scaling by recursively
decomposing tasks into scoped nodes and merging them through compact
continuation memory, while just-in-time context compilation turns each node's
local working context into that memory to keep scaling cost-efficient.

Use Spine as a recursive task-boundary workflow. The Spine tree is the semantic
scope structure for task decomposition and context compilation. Preserve node
hierarchy carefully: every transition must route work to its correct child,
sibling, parent, or ancestor scope.

1. Start task work with `open(<concrete task goal>)` under the startup node.
2. At every node, maintain orientation to the big picture: the current node, its
   parent goal, its role in the parent decomposition, completed siblings,
   remaining siblings, and where the next work belongs.
3. If the current node is unclear, too broad, or not concrete enough to verify,
   use `open(<concrete child goal for exploration, planning, or decomposition>)`
   only when that goal is a true child of the current node. Use the child to
   gather evidence, clarify constraints, plan, or decompose the work. Repeat
   recursively until the next work can be executed in a focused, specific,
   verifiable leaf node.
4. When an exploration/planning/decomposition node is complete, use
   `next(<concrete sibling goal>, memory)` if the next work is a true sibling
   under the same parent. Use `close(memory)` if the distilled result should
   return to the parent before deciding the next node.
5. Use `next(<concrete sibling goal>, memory)` for remaining sibling work under
   the same parent. `next` finalizes the current node and continues in a fresh
   sibling with distilled continuation memory.
6. Use `close(memory)` when the current task node is complete enough for its
   parent or later siblings to continue correctly. `close` is the upward merge
   operation: it returns compact state to the parent, not the local trace. If
   the next work belongs to an ancestor's scope, close upward until the correct
   parent scope is reached, then continue with `next` or `open`.

Optimize the tree for correct progress per unit of working context. Node summaries
should name concrete goals. Node memory should be the minimal sufficient context
needed for correct continuation.

Hierarchy and placement rules:

* Before any `open`, `next`, or `close`, identify the current node's parent goal,
  the current node's role in that parent, and whether the next work belongs to
  the current node, the same parent, or an ancestor scope.
* Use `open` only when the new goal is truly a child of the current node.
* Use `next` only when the new goal is truly a sibling under the same parent.
* Use `close` when the remaining work belongs to the parent or to an ancestor's
  scope; if necessary, close upward before continuing.
* If multiple ancestor levels must be exited, close one level per assistant
  response until the correct scope is reached.
* Every `next` or `close` memory must preserve compact big-picture state:
  current position, parent goal, completed siblings, remaining siblings, key
  decisions/evidence, unresolved risks, and why the transition is
  child/sibling/parent/ancestor-level.

Conventions:
* A single assistant response may batch ordinary task-progress tool calls with at
  most one Spine transition. Never include more than one of `open`, `next`, or
  `close` in the same assistant response.
* `summary` is the concise goal summary for the node being opened: for `open`,
  the child goal; for `next`, the next sibling goal.
* `memory` is concise continuation state with progress, big-picture position,
  decisions, evidence, constraints, risks, remaining work, and critical
  references.
* Optimize for compact recoverability: preserve the smallest sufficient state
  that lets future work continue correctly without replaying this node. Treat
  inherited context and assembled child memory as already available, then write
  only compact deltas and current state needed to continue correctly.
* Use `open` to start child work, `close` to return completed evidence to the
  parent, and `next` to finish the current node and continue from distilled
  memory in a fresh sibling.
* `spine.tree` is read-only; actual transitions happen only through `open`,
  `close`, and `next`.
* Root-epoch ids such as `1` or `2` cannot be closed. The initial `1.1` is a
  startup work node, not a concrete task node; use `open` before doing task work.
* `<spine_status>` gives current node orientation; `<spine_memory>` gives
  continuation memory from closed work.
* `[U#]` anchors refer to numbered user requests. When writing memory, preserve
  `[U#]` anchors and record each request's status. After `<spine_memory>`
  continuity or a node transition, use that record to report only new results,
  blockers, or requested details.
  Treat unresolved `[U#]` requests, pending deliverables, and next actions
  recorded in `<spine_memory>` as continuation obligations; before final or a
  scope transition, reconcile them with the latest user message and answer the
  concrete deliverable rather than drifting to a broader node summary.
* Place user-facing replies where they are most useful: local intermediate
  results may wait for later merge, while complete conclusions, blocking status,
  or decisions needing user input should be surfaced promptly.

</spine_view>
"#;

const SPINE_VIEW_INSTRUCTIONS_OVERRIDE_FILENAME: &str = "spine_instruction.md";
const SPINE_VIEW_START_MARKER: &str = "\n\n<spine_view>";

pub(crate) fn read_spine_instruction_override(
    codex_home: &Path,
    dev_debug_prompt_overrides: bool,
) -> Option<String> {
    if !cfg!(debug_assertions) || !dev_debug_prompt_overrides {
        return None;
    }

    let override_path = codex_home.join(SPINE_VIEW_INSTRUCTIONS_OVERRIDE_FILENAME);
    match std::fs::read_to_string(override_path) {
        Ok(contents) if !contents.trim().is_empty() => Some(contents),
        _ => None,
    }
}

pub(crate) fn append_spine_view_instructions(
    mut base_instructions: String,
    spine_jit_enabled: bool,
    _spine_trim_enabled: bool,
    codex_home: &Path,
    dev_debug_prompt_overrides: bool,
) -> String {
    if !spine_jit_enabled {
        return base_instructions;
    }

    if let Some(start) = base_instructions.rfind(SPINE_VIEW_START_MARKER) {
        base_instructions.truncate(start);
    }

    let override_contents = read_spine_instruction_override(codex_home, dev_debug_prompt_overrides);
    let instructions = override_contents
        .as_deref()
        .and_then(|contents| {
            let start_marker = "<spine_view>";
            let end_marker = "</spine_view>";
            let start = contents.find(start_marker)?;
            let body_start = start.checked_add(start_marker.len())?;
            let relative_end = contents.get(body_start..)?.find(end_marker)?;
            let body_end = body_start.checked_add(relative_end)?;
            let end = body_end.checked_add(end_marker.len())?;
            Some(contents.get(start..end)?.trim().to_string())
        })
        .unwrap_or_else(|| SPINE_JIT_INSTRUCTIONS.to_string());

    if base_instructions.contains(&instructions) {
        return base_instructions;
    }

    if !base_instructions.is_empty() {
        base_instructions.push_str("\n\n");
    }
    base_instructions.push_str(&instructions);
    base_instructions
}

#[cfg(test)]
#[path = "instructions_tests.rs"]
mod tests;
