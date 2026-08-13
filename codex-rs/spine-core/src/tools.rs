use crate::Feature;
use crate::SpawnTask;
use crate::SpineConfig;
use crate::TrimRequest;
use crate::config::MAX_MODEL_VISIBLE_TEXT_BYTES;
use serde::Deserialize;
use serde_json::Value;
use std::fmt;

pub const MAX_SUMMARY_BYTES: usize = 4 * 1024;
pub const MAX_MEMORY_BYTES: usize = 32 * 1024;
pub const MAX_SPAWN_TASKS: usize = 16;
/// A Spawn task's assignment is later embedded in one child `UserInput::Text`.
/// The host also checks the fully rendered envelope (including the peer roster)
/// before admission; this field cap prevents an unbounded raw assignment from
/// reaching that second boundary.
pub const MAX_SPAWN_PROMPT_BYTES: usize = MAX_MODEL_VISIBLE_TEXT_BYTES;
pub const MAX_SPAWN_BATCH_BYTES: usize = 64 * 1024;

pub const SPINE_NAMESPACE: &str = "spine";
pub const SPINE_NAMESPACE_DESCRIPTION: &str = concat!(
    "Use Spine to shape the work. The `memory` field shared by close and next is model-authored continuation state for replacing the finalized node's local working context. ",
    "Preserve only what later work needs beyond inherited context: completed or confirmed progress, confirmed findings, decisions and constraints, validation results, bounded unresolved factual gaps or risks, remaining work that can proceed from this memory and inherited context without reconstructing the replaced working context, and the logic linking evidence and findings to decisions and next steps. ",
    "Include compact supporting evidence or precise, recoverable references when needed. ",
    "For source code, cite exact paths and lines; for commands, cite the exact command and decisive output or result, so continuation need not replay the work. ",
    "Runtime preserves user messages and child memories. ",
    "Use existing `[U#]` anchors only to bind approvals, corrections, rejections, clarifications, and elliptical replies to their referents and record the resulting continuation-relevant semantic deltas in task scope, decisions, constraints, progress, and remaining obligations; the underlying user messages remain available independently of these references."
);
const NODE_MEMORY_DESCRIPTION: &str =
    "Continuation state following the shared memory contract in the Spine namespace description.";
const OPEN_GOAL_DESCRIPTION: &str = "Concise, actionable, independently completable outcome owned by the direct child. The call carrying this goal remains in the child's context.";
const NEXT_GOAL_DESCRIPTION: &str = "Concise, actionable, independently completable outcome owned by the true sibling. The call carrying this goal remains in the sibling's context; finalized state stays in memory.";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SpineTool {
    Open,
    Close,
    Next,
    Trim,
    Spawn,
}

impl SpineTool {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Close => "close",
            Self::Next => "next",
            Self::Trim => "trim",
            Self::Spawn => "spawn",
        }
    }

    pub fn qualified_name(self) -> String {
        format!("{SPINE_NAMESPACE}.{}", self.name())
    }

    pub const fn feature(self) -> Feature {
        match self {
            Self::Open | Self::Close | Self::Next => Feature::Jit,
            Self::Trim => Feature::Trim,
            Self::Spawn => Feature::Spawn,
        }
    }

    pub const fn all() -> [Self; 5] {
        [Self::Open, Self::Close, Self::Next, Self::Spawn, Self::Trim]
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolDefinition {
    pub tool: SpineTool,
    pub description: String,
    pub parameters: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolCatalog {
    definitions: Vec<ToolDefinition>,
}

impl ToolCatalog {
    pub fn new(config: &SpineConfig) -> Result<Self, crate::InitError> {
        config.validate()?;
        let definitions = SpineTool::all()
            .into_iter()
            .filter(|tool| config.is_enabled(tool.feature()))
            .map(|tool| ToolDefinition {
                tool,
                description: config
                    .tool_description(tool.name())
                    .unwrap_or_default()
                    .to_string(),
                parameters: parameters_for(tool),
            })
            .collect();
        Ok(Self { definitions })
    }

    pub fn with_spawn_max_items(mut self, max_items: usize) -> Self {
        if max_items < 2 {
            self.definitions
                .retain(|definition| definition.tool != SpineTool::Spawn);
            return self;
        }

        if let Some(definition) = self
            .definitions
            .iter_mut()
            .find(|definition| definition.tool == SpineTool::Spawn)
        {
            let max_items = max_items.min(MAX_SPAWN_TASKS);
            definition.parameters["properties"]["tasks"]["maxItems"] = serde_json::json!(max_items);
            let description = definition
                .description
                .split(" The tasks array must contain at least ")
                .next()
                .unwrap_or(&definition.description);
            definition.description = format!(
                "{description} The tasks array must contain at least 2 and at most {max_items} task assignments."
            );
        }
        self
    }

    pub fn validate(
        &self,
        tool: SpineTool,
        arguments: &str,
    ) -> Result<ToolValidation, ToolValidationError> {
        validate_tool(tool, arguments)
    }

    pub fn definitions(&self) -> &[ToolDefinition] {
        &self.definitions
    }

    pub fn definition(&self, tool: SpineTool) -> Option<&ToolDefinition> {
        self.definitions
            .iter()
            .find(|definition| definition.tool == tool)
    }

    pub fn names(&self) -> Vec<String> {
        self.definitions
            .iter()
            .map(|definition| definition.tool.qualified_name())
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ToolValidation {
    Ordinary,
    Transition(ValidatedTransition),
}

#[derive(Clone, Debug, PartialEq)]
pub enum ValidatedTransition {
    Open { summary: String },
    Close { memory: String },
    Next { summary: String, memory: String },
    Trim(TrimRequest),
    Spawn { tasks: Vec<SpawnTask> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolValidationError {
    InvalidJson(String),
    UnknownTool(String),
    EmptyField(&'static str),
    FieldTooLarge {
        field: &'static str,
        max_bytes: usize,
    },
    InvalidTrim(String),
    InvalidSpawn(String),
}

impl fmt::Display for ToolValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidJson(error) => write!(formatter, "invalid Spine tool arguments: {error}"),
            Self::UnknownTool(name) => write!(formatter, "unknown Spine tool {name}"),
            Self::EmptyField(name) => write!(formatter, "Spine tool field {name} is empty"),
            Self::FieldTooLarge { field, max_bytes } => {
                write!(
                    formatter,
                    "Spine tool field {field} exceeds {max_bytes} bytes"
                )
            }
            Self::InvalidTrim(error) => write!(formatter, "invalid spine.trim arguments: {error}"),
            Self::InvalidSpawn(error) => {
                write!(formatter, "invalid spine.spawn arguments: {error}")
            }
        }
    }
}

impl std::error::Error for ToolValidationError {}

pub fn validate_tool(
    tool: SpineTool,
    arguments: &str,
) -> Result<ToolValidation, ToolValidationError> {
    validate_tool_with_spawn_limit(tool, arguments, MAX_SPAWN_TASKS)
}

fn validate_tool_with_spawn_limit(
    tool: SpineTool,
    arguments: &str,
    spawn_task_limit: usize,
) -> Result<ToolValidation, ToolValidationError> {
    match tool {
        SpineTool::Open => {
            let args: OpenArgs = parse_control(arguments)?;
            Ok(ToolValidation::Transition(ValidatedTransition::Open {
                summary: bounded_non_empty(args.summary, "summary", MAX_SUMMARY_BYTES)?,
            }))
        }
        SpineTool::Close => {
            let args: CloseArgs = parse_control(arguments)?;
            Ok(ToolValidation::Transition(ValidatedTransition::Close {
                memory: bounded_non_empty(args.memory, "memory", MAX_MEMORY_BYTES)?,
            }))
        }
        SpineTool::Next => {
            let args: NextArgs = parse_control(arguments)?;
            Ok(ToolValidation::Transition(ValidatedTransition::Next {
                summary: bounded_non_empty(args.summary, "summary", MAX_SUMMARY_BYTES)?,
                memory: bounded_non_empty(args.memory, "memory", MAX_MEMORY_BYTES)?,
            }))
        }
        SpineTool::Trim => TrimRequest::parse(arguments)
            .map(ValidatedTransition::Trim)
            .map(ToolValidation::Transition)
            .map_err(ToolValidationError::InvalidTrim),
        SpineTool::Spawn => {
            let args: SpawnArgs = parse_control(arguments)?;
            if args.tasks.len() < 2 {
                return Err(ToolValidationError::InvalidSpawn(
                    "spine.spawn requires at least two tasks".to_string(),
                ));
            }
            if args.tasks.len() > spawn_task_limit {
                return Err(ToolValidationError::InvalidSpawn(format!(
                    "spine.spawn accepts at most {spawn_task_limit} tasks"
                )));
            }
            let aggregate_bytes = args.tasks.iter().fold(0usize, |total, task| {
                total
                    .saturating_add(task.summary.len())
                    .saturating_add(task.prompt.len())
            });
            if aggregate_bytes > MAX_SPAWN_BATCH_BYTES {
                return Err(ToolValidationError::InvalidSpawn(format!(
                    "spine.spawn task payload exceeds {MAX_SPAWN_BATCH_BYTES} bytes"
                )));
            }
            for task in &args.tasks {
                bounded_non_empty(task.summary.clone(), "summary", MAX_SUMMARY_BYTES)?;
                bounded_non_empty(task.prompt.clone(), "prompt", MAX_SPAWN_PROMPT_BYTES)?;
            }
            Ok(ToolValidation::Transition(ValidatedTransition::Spawn {
                tasks: args.tasks,
            }))
        }
    }
}

pub const fn success_carrier(tool: SpineTool) -> Option<&'static str> {
    match tool {
        SpineTool::Open => Some("Spine open accepted."),
        SpineTool::Close => Some("Spine close accepted."),
        SpineTool::Next => Some("Spine next accepted."),
        SpineTool::Trim => Some("Spine trim accepted."),
        SpineTool::Spawn => None,
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenArgs {
    summary: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CloseArgs {
    memory: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NextArgs {
    summary: String,
    memory: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpawnArgs {
    tasks: Vec<SpawnTask>,
}

fn parse_control<T: for<'de> Deserialize<'de>>(arguments: &str) -> Result<T, ToolValidationError> {
    serde_json::from_str(arguments)
        .map_err(|error| ToolValidationError::InvalidJson(error.to_string()))
}

fn bounded_non_empty(
    value: String,
    field: &'static str,
    max_bytes: usize,
) -> Result<String, ToolValidationError> {
    let value = value.trim().to_string();
    if value.is_empty() {
        return Err(ToolValidationError::EmptyField(field));
    }
    if value.len() > max_bytes {
        return Err(ToolValidationError::FieldTooLarge { field, max_bytes });
    }
    Ok(value)
}

fn parameters_for(tool: SpineTool) -> Value {
    match tool {
        SpineTool::Open => serde_json::json!({
            "type": "object",
            "properties": { "summary": { "type": "string", "maxLength": MAX_SUMMARY_BYTES, "description": OPEN_GOAL_DESCRIPTION } },
            "required": ["summary"],
            "additionalProperties": false
        }),
        SpineTool::Close => serde_json::json!({
            "type": "object",
            "properties": { "memory": { "type": "string", "maxLength": MAX_MEMORY_BYTES, "description": NODE_MEMORY_DESCRIPTION } },
            "required": ["memory"],
            "additionalProperties": false
        }),
        SpineTool::Next => serde_json::json!({
            "type": "object",
            "properties": {
                "summary": { "type": "string", "description": NEXT_GOAL_DESCRIPTION },
                "memory": { "type": "string", "description": NODE_MEMORY_DESCRIPTION }
            },
            "required": ["summary", "memory"],
            "additionalProperties": false
        }),
        SpineTool::Trim => serde_json::json!({
            "type": "object",
            "properties": {
                "TRIM_ID": { "type": "string", "description": "Trim id attached to a tool response in the immediately previous tool-result batch; it expires after your next assistant tool request." },
                "op": { "type": "string", "enum": ["snip", "slice"], "description": "Use snip only when useful facts are preserved elsewhere; use slice to keep the needed head, tail, or anchor window." },
                "head": { "type": "integer", "description": "For op=\"slice\", keep this many characters from the start of the current visible body. Mutually exclusive with tail and anchor." },
                "tail": { "type": "integer", "description": "For op=\"slice\", keep this many characters from the end of the current visible body. Mutually exclusive with head and anchor." },
                "anchor": { "type": "string", "description": "For op=\"slice\", locate this non-empty text in the current visible body and keep an anchor window. Mutually exclusive with head and tail." },
                "preceding": { "type": "integer", "description": "For anchor slice, keep this many complete lines before the anchor line." },
                "following": { "type": "integer", "description": "For anchor slice, keep this many complete lines after the anchor line." }
            },
            "required": ["TRIM_ID", "op"],
            "additionalProperties": false
        }),
        SpineTool::Spawn => serde_json::json!({
            "type": "object",
            "properties": {
                "tasks": {
                    "type": "array",
                    "description": "Ordered differentiated branch assignments.",
                    "minItems": 2,
                    "items": {
                        "type": "object",
                        "properties": {
                            "summary": { "type": "string", "maxLength": MAX_SUMMARY_BYTES, "description": "Concise branch label, distinct within this spawn call, and its independently owned outcome." },
                            "prompt": { "type": "string", "maxLength": MAX_SPAWN_PROMPT_BYTES, "description": "Complete initial branch assignment. The branch identity is this task's summary. Include the same task-local `Shared blackboard: <path>` line used by every branch so they can coordinate, share useful findings, and reduce duplicated exploration." }
                        },
                        "required": ["summary", "prompt"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["tasks"],
            "additionalProperties": false
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_catalog_is_feature_gated() {
        let config = SpineConfig::v1().with_feature(Feature::Jit).unwrap();
        let catalog = ToolCatalog::new(&config).unwrap();
        assert_eq!(catalog.names(), ["spine.open", "spine.close", "spine.next"]);
        assert!(catalog.definition(SpineTool::Trim).is_none());
    }

    #[test]
    fn feature_off_catalog_is_empty() {
        let catalog = ToolCatalog::new(&SpineConfig::v1()).unwrap();
        assert!(catalog.definitions().is_empty());
    }

    #[test]
    fn model_visible_tool_schema_strings_are_hard_bounded() {
        fn assert_bounded(value: &Value) {
            match value {
                Value::String(value) => {
                    assert!(value.len() <= MAX_MODEL_VISIBLE_TEXT_BYTES)
                }
                Value::Array(values) => values.iter().for_each(assert_bounded),
                Value::Object(values) => values.values().for_each(assert_bounded),
                Value::Bool(_) | Value::Null | Value::Number(_) => {}
            }
        }

        let config = SpineConfig::v1()
            .with_features([Feature::Jit, Feature::Trim, Feature::Spawn])
            .unwrap();
        let catalog = ToolCatalog::new(&config).unwrap().with_spawn_max_items(16);
        assert!(SPINE_NAMESPACE_DESCRIPTION.len() > 1024);
        assert!(SPINE_NAMESPACE_DESCRIPTION.len() <= MAX_MODEL_VISIBLE_TEXT_BYTES);
        for definition in catalog.definitions() {
            assert!(definition.description.len() <= MAX_MODEL_VISIBLE_TEXT_BYTES);
            assert_bounded(&definition.parameters);
        }
    }

    #[test]
    fn spawn_schema_specialization_encodes_runtime_capacity() {
        let config = SpineConfig::v1()
            .with_features([Feature::Jit, Feature::Spawn])
            .unwrap();
        let catalog = ToolCatalog::new(&config).unwrap().with_spawn_max_items(5);
        let definition = catalog.definition(SpineTool::Spawn).unwrap();
        assert_eq!(
            definition.parameters["properties"]["tasks"]["maxItems"],
            serde_json::json!(5)
        );
    }

    #[test]
    fn spawn_schema_specialization_hides_unusable_spawn() {
        let config = SpineConfig::v1()
            .with_features([Feature::Jit, Feature::Spawn])
            .unwrap();
        let catalog = ToolCatalog::new(&config).unwrap().with_spawn_max_items(1);
        assert!(catalog.definition(SpineTool::Spawn).is_none());
    }

    #[test]
    fn validators_reject_malformed_controls_and_spawn_vectors() {
        assert!(validate_tool(SpineTool::Open, r#"{"summary":" task "}"#).is_ok());
        assert!(validate_tool(SpineTool::Close, r#"{"memory":" "}"#).is_err());
        assert!(validate_tool(SpineTool::Open, r#"{"summary":"x","extra":1}"#).is_err());
        assert!(validate_tool(SpineTool::Spawn, r#"{"tasks":[]}"#).is_err());
    }

    #[test]
    fn validators_reject_unbounded_context_fields() {
        let oversized_summary = serde_json::json!({
            "summary": "x".repeat(MAX_SUMMARY_BYTES + 1)
        });
        assert!(matches!(
            validate_tool(SpineTool::Open, &oversized_summary.to_string()),
            Err(ToolValidationError::FieldTooLarge { .. })
        ));
        let oversized_memory = serde_json::json!({
            "memory": "x".repeat(MAX_MEMORY_BYTES + 1)
        });
        assert!(matches!(
            validate_tool(SpineTool::Close, &oversized_memory.to_string()),
            Err(ToolValidationError::FieldTooLarge { .. })
        ));
        let oversized_unicode = serde_json::json!({
            "summary": "界".repeat(MAX_SUMMARY_BYTES / 2 + 1)
        });
        assert!(matches!(
            validate_tool(SpineTool::Open, &oversized_unicode.to_string()),
            Err(ToolValidationError::FieldTooLarge { .. })
        ));
    }

    #[test]
    fn validators_accept_exact_limits_and_reject_the_next_spawn_task() {
        let next = serde_json::json!({
            "summary": "s".repeat(MAX_SUMMARY_BYTES),
            "memory": "m".repeat(MAX_MEMORY_BYTES),
        });
        assert!(validate_tool(SpineTool::Next, &next.to_string()).is_ok());

        let mut tasks = (0..MAX_SPAWN_TASKS)
            .map(|_| serde_json::json!({"summary": "s", "prompt": "p"}))
            .collect::<Vec<_>>();
        tasks[0] = serde_json::json!({
            "summary": "s".repeat(MAX_SUMMARY_BYTES),
            "prompt": "p".repeat(MAX_SPAWN_PROMPT_BYTES),
        });
        let arguments = serde_json::json!({"tasks": tasks}).to_string();
        assert!(validate_tool(SpineTool::Spawn, &arguments).is_ok());

        let oversized = (0..=MAX_SPAWN_TASKS)
            .map(|_| serde_json::json!({"summary": "s", "prompt": "p"}))
            .collect::<Vec<_>>();
        let arguments = serde_json::json!({"tasks": oversized}).to_string();
        assert!(matches!(
            validate_tool(SpineTool::Spawn, &arguments),
            Err(ToolValidationError::InvalidSpawn(_))
        ));
    }

    #[test]
    fn validators_reject_aggregate_spawn_payloads() {
        let tasks = (0..2)
            .map(|_| {
                serde_json::json!({
                    "summary": "s",
                    "prompt": "p".repeat(MAX_SPAWN_BATCH_BYTES / 2)
                })
            })
            .collect::<Vec<_>>();
        let arguments = serde_json::json!({"tasks": tasks}).to_string();

        assert!(matches!(
            validate_tool(SpineTool::Spawn, &arguments),
            Err(ToolValidationError::InvalidSpawn(_))
        ));
    }
}
