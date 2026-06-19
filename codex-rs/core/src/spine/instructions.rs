use std::path::Path;

pub(crate) const SPINE_JIT_INSTRUCTIONS: &str = r#"<spine_view>
Spine helps organize ongoing work, keep the active context useful, compact old
work into memory, make later continuation reliable, and control cost.

Spine organizes ongoing work into task-level nodes. Treat the current node as
the place where present work happens. Manage nodes so the working history stays
useful for reasoning, tool use, compaction, and later continuation.
Treat a node as one compactible work unit: if a later model could resume from a
short memory without replaying the raw trace, that work unit is ready to close
or advance.

- Continue in the current node when it represents the next work at the right
  granularity. Suitability is determined by scope and phase; shared request,
  issue, milestone, or feature is only weak evidence.
- Use `open` when the next work is better handled in a focused child node.
- Use `close` when the current node has useful state worth preserving as memory
  and its full local history is no longer needed.
- Use `next` when the current node should be closed and the next work is better
  handled in a peer node.

Close or next especially when:
- a focused phase of work has produced useful state that memory can preserve;
- the next work is a peer phase and the current phase can be resumed from
  memory;
- the current node has become noisy, stale, repetitive, or dominated by
  irrelevant exploration, so preserving useful state and continuing from memory
  would be cleaner.

Simple one-turn replies can be answered directly without changing Spine.

`open` creates a child. `close` and `next` compact the useful state from the
current working context and reduce future prompt context. Do not wait for
perfect completion when the current useful state is already enough for clean
continuation from memory. The compact memory is authored by you in the `memory`
argument of `close`/`next`; runtime preserves exact user messages and child
memories, then appends your continuation memory. Choose based on whether the
current node remains useful for continued work, not raw context size alone.
If the child already has a user-relevant conclusion, surface it before closing.

Conventions:
- `summary` is a short user-facing label in the conversation language.
- `memory` on `close`/`next` is required. Write concise continuation memory for
  the next LLM that may resume this task: current progress, stable facts,
  decisions, evidence, constraints, unresolved risks, remaining work, and
  critical files, tests, commands, or references. When relevant preserved user
  messages have `[U#]` anchors, cite those anchors and state what was done
  after each request and whether it is completed, partial, blocked, or pending
  at node close. Also record user-visible conclusions or final replies already
  delivered, so a later continuation does not repeat them as new work.
- Before replying after `<spine_memory>` continuity or a node transition, check
  what has already been told to the user and only report new status, changes, or
  requested details.
- `<spine_status>` gives Spine node context and orientation when present.
- `<spine_memory>` provides continuity from closed work.
- Choose at most one of `open`, `close`, or `next` in one assistant response.
- `spine.tree` is a read-only inspector for unclear tree/cursor state.

</spine_view>
"#;

pub(crate) const SPINE_TRIM_INSTRUCTIONS: &str = r#"<spine_trim>
`spine.trim` keeps tagged tool responses small in the visible context. A
`TRIM_ID` is eligible only for the most recent completed toolcall: the tool
request you just made and the tool responses that just came back. After any
later toolcall completes, older `TRIM_ID`s expire.

After reading a tagged tool response, keep the evidence needed for the current
task and trim the rest in your next assistant response that calls tools.
`spine.trim` can be called alongside other useful tools in that response.

Use `slice` to retain a sufficient head, tail, or anchor window. Use `snip`
when the useful facts are already captured in notes, code, tests, or your
response.

If trim misses, treat that `TRIM_ID` as expired.

</spine_trim>
"#;

const SPINE_VIEW_INSTRUCTIONS_OVERRIDE_FILENAME: &str = "spine_instruction.md";
const SPINE_VIEW_START_MARKERS: [&str; 2] = ["\n\n<spine_view>", "\n\n<spine_trim>"];

pub(crate) fn append_spine_view_instructions(
    mut base_instructions: String,
    spine_jit_enabled: bool,
    spine_trim_enabled: bool,
    codex_home: &Path,
    dev_debug_prompt_overrides: bool,
) -> String {
    if !spine_jit_enabled && !spine_trim_enabled {
        return base_instructions;
    }

    let override_contents = if cfg!(debug_assertions) && dev_debug_prompt_overrides {
        let override_path = codex_home.join(SPINE_VIEW_INSTRUCTIONS_OVERRIDE_FILENAME);
        match std::fs::read_to_string(override_path) {
            Ok(contents) if !contents.trim().is_empty() => Some(contents),
            _ => None,
        }
    } else {
        None
    };
    let instructions = joined_spine_instructions(
        spine_jit_enabled,
        spine_trim_enabled,
        override_contents.as_deref(),
    );

    if base_instructions.contains(&instructions) {
        return base_instructions;
    }
    if let Some(start) = SPINE_VIEW_START_MARKERS
        .into_iter()
        .filter_map(|marker| base_instructions.rfind(marker))
        .min()
    {
        base_instructions.truncate(start);
    }

    if !base_instructions.is_empty() {
        base_instructions.push_str("\n\n");
    }
    base_instructions.push_str(&instructions);
    base_instructions
}

fn joined_spine_instructions(
    spine_jit_enabled: bool,
    spine_trim_enabled: bool,
    override_contents: Option<&str>,
) -> String {
    let mut sections = Vec::new();
    if spine_jit_enabled {
        sections.push(
            override_contents
                .and_then(|contents| extract_section(contents, "spine_view"))
                .unwrap_or_else(|| SPINE_JIT_INSTRUCTIONS.to_string()),
        );
    }
    if spine_trim_enabled {
        sections.push(
            override_contents
                .and_then(|contents| extract_section(contents, "spine_trim"))
                .unwrap_or_else(|| SPINE_TRIM_INSTRUCTIONS.to_string()),
        );
    }
    sections.join("\n\n")
}

fn extract_section(contents: &str, tag: &str) -> Option<String> {
    let start_marker = format!("<{tag}>");
    let end_marker = format!("</{tag}>");
    let start = contents.find(&start_marker)?;
    let body_start = start.checked_add(start_marker.len())?;
    let relative_end = contents.get(body_start..)?.find(&end_marker)?;
    let end = body_start
        .checked_add(relative_end)?
        .checked_add(end_marker.len())?;
    Some(contents.get(start..end)?.trim().to_string())
}

#[cfg(test)]
#[path = "instructions_tests.rs"]
mod tests;
