use std::path::Path;

pub(crate) const SPINE_JIT_INSTRUCTIONS: &str = r#"<spine_view>
Use Spine as a recursive work tree with the **smallest sufficient working
context**.

Each Spine node should pursue one clear, bounded, completable goal. If the goal
is unclear, first explore only enough to define such a goal or decompose it into
concrete sub-goals.

Work recursively:
* If the active goal depends on sub-goals, use `next` when needed to carry
  forward distilled memory, then `open` child nodes for those sub-goals and
  close/merge their memories before closing the parent.
* If the active goal is directly actionable, do the work and verify it with
  available evidence.
* When the active goal is complete, use `close` if the parent should merge or
  decide next steps; use `next` only when the next sibling goal is already
  clear.
* If a goal grows beyond one bounded, completable goal, treat that as a new
  unclear goal and decompose recursively.

Conventions:
* Prefer batching Spine tools with ordinary task-progress tool calls in the same assistant tool request.
* `summary` is the concise goal summary for the node being opened: for `open`,
  the child goal; for `next`, the next sibling goal. `memory` is concise
  continuation state with progress, decisions, evidence, constraints, risks,
  remaining work, and critical references.
* Use `open` to start child work, `close` to return completed evidence to the
  parent, and `next` to finish the current node and continue from distilled
  memory in a fresh sibling.
* Use at most one of `open`, `close`, or `next` in one assistant response.
  `spine.tree` is read-only; actual transitions happen only through `open`,
  `close`, and `next`.
* Root-epoch ids such as `1` or `2` cannot be closed. For substantive
  Spine-managed work, the initial `1.1` is a startup work node, not a concrete
  task node; use `open` before doing substantive task work.
* `<spine_status>` gives current node orientation; `<spine_memory>` gives
  continuation memory from closed work.
* When writing memory, preserve `[U#]` anchors and record each request's status.
  After `<spine_memory>` continuity or a node transition, use that record to
  report only new results, blockers, or requested details.
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

    strip_appended_spine_sections(&mut base_instructions);

    let override_contents = read_spine_instruction_override(codex_home, dev_debug_prompt_overrides);
    let instructions = joined_spine_instructions(spine_jit_enabled, override_contents.as_deref());

    if base_instructions.contains(&instructions) {
        return base_instructions;
    }

    append_block(base_instructions, &instructions)
}

fn strip_appended_spine_sections(base_instructions: &mut String) {
    if let Some(start) = base_instructions.rfind(SPINE_VIEW_START_MARKER) {
        base_instructions.truncate(start);
    }
}

fn joined_spine_instructions(spine_jit_enabled: bool, override_contents: Option<&str>) -> String {
    let mut sections = Vec::new();
    if spine_jit_enabled {
        sections.push(
            override_contents
                .and_then(|contents| extract_section(contents, "spine_view"))
                .unwrap_or_else(|| SPINE_JIT_INSTRUCTIONS.to_string()),
        );
    }
    sections.join("\n\n")
}

fn extract_section(contents: &str, tag: &str) -> Option<String> {
    let (start, _, _, end) = extract_section_bounds(contents, tag)?;
    Some(contents.get(start..end)?.trim().to_string())
}

fn extract_section_bounds(contents: &str, tag: &str) -> Option<(usize, usize, usize, usize)> {
    let start_marker = format!("<{tag}>");
    let end_marker = format!("</{tag}>");
    let start = contents.find(&start_marker)?;
    let body_start = start.checked_add(start_marker.len())?;
    let relative_end = contents.get(body_start..)?.find(&end_marker)?;
    let body_end = body_start.checked_add(relative_end)?;
    let end = body_end.checked_add(end_marker.len())?;
    Some((start, body_start, body_end, end))
}

fn append_block(mut base_instructions: String, block: &str) -> String {
    if !base_instructions.is_empty() {
        base_instructions.push_str("\n\n");
    }
    base_instructions.push_str(block);
    base_instructions
}

#[cfg(test)]
#[path = "instructions_tests.rs"]
mod tests;
