use std::path::Path;
use std::str::FromStr;

pub(crate) const SPINE_JIT_INSTRUCTIONS: &str = r#"<spine_view>
All work is actively managed by Spine tools to keep the **smallest sufficient
working context** for efficient and effective task resolution.
For anything beyond a direct answer in the current response, put the work under
Spine control: plan with `tree`, execute focused work in leaf nodes, and carry
completed phases forward with `close`/`next`.

Use Spine in recursive EE mode: exploration -> exploitation. When the work is
still unclear, open a bounded exploration node to discover the work phase.
When the work phase becomes actionable, use `next` to fold exploration into
memory and continue in a fresh node. There, use `tree` to maintain the local
worktree plan, including upcoming nodes, and `open` known child nodes down to
focused leaf work.

If a leaf grows into a harder or broader problem, use `next` to carry that
discovery into a fresh local work phase, then repeat EE mode from there. Use
`close` when a leaf has produced the result its parent needs.

`open` starts known child work inside the current work phase. `close` folds
completed child work into memory for its parent. `next` folds the current work
phase into memory and starts a fresh sibling phase. When memory can carry the
useful state forward, transition promptly so later work depends on memory rather
than retained history.

Choose work-phase boundaries that keep live context small and memory useful.
Prefer `next` after exploration produces an actionable work phase, and prefer
`close` after focused child work produces the result its parent needs. If context
pressure grows, use the next EE boundary to carry completed state forward before
global compaction is forced. Global compaction may lose Spine tree state, so
later work may have to reorganize from a new root.

Root-epoch ids such as `1` or `2` cannot be closed; the initial `1.1` is a
startup work node, not a concrete task node, so use `open` first before
substantive work. Place user-facing replies at the node where they are most
useful: local intermediate results may wait for later merge, while complete
conclusions, blocking status, or information needing user decision should be
surfaced promptly.

Conventions:
- `summary` is a short label in the user's language.
- `memory` on `close`/`next` is required. Write concise continuation state for
  the next LLM: progress, stable facts, decisions, evidence, constraints,
  unresolved risks, remaining work, and critical files, tests, commands, or
  references. When preserved user messages have `[U#]` anchors, cite them and
  mark each request as completed, partial, blocked, or pending. Record what has
  already been told to the user so later continuation does not repeat it as new
  work.
- Before replying after `<spine_memory>` continuity or a node transition, check
  what has already been told to the user and only report new status, changes, or
  requested details.
- `<spine_status>` gives current node context and orientation.
- `<spine_memory>` gives continuation state from closed work.
- Use at most one of `open`, `close`, or `next` in one assistant response.
- `spine.tree` shows the committed task tree, cursor, and any ongoing/future
  node plan. Actual tree transitions happen only through `open`, `close`, and
  `next`.

</spine_view>
"#;

pub(crate) const SPINE_TRIM_INSTRUCTIONS: &str = r#"<spine_trim>
`spine.trim` keeps tagged tool responses to the smallest sufficient evidence for
the current work.

A trim window is the immediately previous tool-result batch: the tool responses
returned from your last assistant tool request. A `TRIM_ID` is live only in that
batch. If that request returned multiple tagged responses, all tagged responses
in the batch can be trimmed. Once any later tool request completes, that
previous batch's `TRIM_ID`s expire.

After reading a tagged tool-result batch, preserve the evidence needed to
continue and use `spine.trim` in your next assistant tool request, optionally
batched with other useful tools. Use only `TRIM_ID`s from the latest returned
tool-result batch.

Use `slice` to keep a sufficient head, tail, or anchor window. Use `snip` when
the useful facts are already captured in memory, notes, code, tests, files, tool
arguments, or your response.

If trim misses, treat that `TRIM_ID` as expired and continue.

</spine_trim>
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpineScalingLevel {
    Low,
    Medium,
    High,
    Auto,
}

impl FromStr for SpineScalingLevel {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "auto" => Ok(Self::Auto),
            other => Err(format!(
                "unsupported Spine scaling level {other:?}; use low, medium, high, or auto"
            )),
        }
    }
}

const SPINE_VIEW_INSTRUCTIONS_OVERRIDE_FILENAME: &str = "spine_instruction.md";
const SPINE_VIEW_START_MARKERS: [&str; 2] = ["\n\n<spine_view>", "\n\n<spine_trim>"];

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

pub(crate) fn extract_spine_instruction_section_body(contents: &str, tag: &str) -> Option<String> {
    extract_section_body(contents, tag)
}

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

    let override_contents = read_spine_instruction_override(codex_home, dev_debug_prompt_overrides);
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

pub(crate) fn append_spine_scaling_instructions(
    mut base_instructions: String,
    spine_scaling: Option<SpineScalingLevel>,
    codex_home: &Path,
    dev_debug_prompt_overrides: bool,
) -> String {
    let Some(spine_scaling) = spine_scaling else {
        return base_instructions;
    };
    let override_contents = read_spine_instruction_override(codex_home, dev_debug_prompt_overrides);
    let Some(block) = spine_scaling_prompt_block(spine_scaling, override_contents.as_deref())
    else {
        return base_instructions;
    };
    if !base_instructions.is_empty() {
        base_instructions.push_str("\n\n");
    }
    base_instructions.push_str(&block);
    base_instructions
}

pub(crate) fn spine_scaling_prompt_block(
    spine_scaling: SpineScalingLevel,
    override_contents: Option<&str>,
) -> Option<String> {
    let tag = spine_scaling_override_tag(spine_scaling)?;
    override_contents
        .and_then(|contents| extract_spine_instruction_section_body(contents, tag))
        .or_else(|| default_spine_scaling_prompt_block(spine_scaling).map(str::to_string))
}

fn spine_scaling_override_tag(spine_scaling: SpineScalingLevel) -> Option<&'static str> {
    match spine_scaling {
        SpineScalingLevel::Low => None,
        SpineScalingLevel::Medium => Some("spine_scaling_medium"),
        SpineScalingLevel::High => Some("spine_scaling_high"),
        SpineScalingLevel::Auto => Some("spine_scaling_auto"),
    }
}

fn default_spine_scaling_prompt_block(spine_scaling: SpineScalingLevel) -> Option<&'static str> {
    match spine_scaling {
        SpineScalingLevel::Low => None,
        SpineScalingLevel::Medium => Some(
            r#"<spine_scaling>
Task-level scaling: medium.
Use `spine.tree` as the planning surface before substantial work: decompose the
task into a shallow future node plan, solve focused parts in separate nodes,
then merge resolved work upward through memory.
Budget: depth 2 x branch 2; plan up to 4 focused leaves.
Prefer `close`/`next` after each resolved phase so progress accumulates by
summarized state, not retained history.
</spine_scaling>"#,
        ),
        SpineScalingLevel::High => Some(
            r#"<spine_scaling>
Task-level scaling: high.
Use `spine.tree` as the task-level test-time-scaling controller before
substantial work: decompose the problem thoroughly, choose tree depth from
complexity and uncertainty, solve focused leaves independently, and merge
findings upward through memory.
Budget: depth 3 x branch 3; plan up to 27 focused leaves.
Revise the future tree as evidence changes; open deeper nodes for hard blockers
or independent phases, and reduce completed leaves with `close`/`next` before
broadening.
</spine_scaling>"#,
        ),
        SpineScalingLevel::Auto => Some(
            r#"<spine_scaling>
Task-level scaling: auto.
Budget: choose reasonable depth and branch count for the task.
</spine_scaling>"#,
        ),
    }
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

fn extract_section_body(contents: &str, tag: &str) -> Option<String> {
    let start_marker = format!("<{tag}>");
    let end_marker = format!("</{tag}>");
    let start = contents.find(&start_marker)?;
    let body_start = start.checked_add(start_marker.len())?;
    let relative_end = contents.get(body_start..)?.find(&end_marker)?;
    let body_end = body_start.checked_add(relative_end)?;
    Some(contents.get(body_start..body_end)?.trim().to_string())
}

#[cfg(test)]
#[path = "instructions_tests.rs"]
mod tests;
