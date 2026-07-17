use serde::Deserialize;
use serde::Serialize;
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RawBoundary(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawSpan {
    pub start: RawBoundary,
    pub end: RawBoundary,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct NodeId(Vec<u32>);

impl NodeId {
    pub fn root_epoch(epoch: u32) -> Self {
        Self(vec![epoch])
    }

    pub fn child(&self, ordinal: u32) -> Self {
        let mut parts = self.0.clone();
        parts.push(ordinal);
        Self(parts)
    }

    pub fn parts(&self) -> &[u32] {
        &self.0
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, part) in self.0.iter().enumerate() {
            if index > 0 {
                f.write_str(".")?;
            }
            write!(f, "{part}")?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageRole {
    User,
    Assistant,
    Developer,
    System,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub boundary: RawBoundary,
    pub role: MessageRole,
    pub content: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolOutcome {
    Succeeded,
    Failed,
    Unknown,
}

pub const SPINE_SPAWN_RESULT_SCHEMA: &str = "spine.spawn.result.v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpawnTask {
    pub summary: String,
    pub prompt: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpawnOutcome {
    Completed,
    Errored,
    Aborted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpawnResult {
    pub ordinal: u32,
    pub outcome: SpawnOutcome,
    pub memory_body: String,
    #[serde(default)]
    pub diagnostic: Option<String>,
    #[serde(default)]
    pub execution_ref: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpawnReceipt {
    pub schema: String,
    pub results: Vec<SpawnResult>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpawnValidationError {
    TooFewTasks,
    EmptyTaskSummary { ordinal: usize },
    EmptyTaskPrompt { ordinal: usize },
    InvalidSchema { schema: String },
    ResultCount { expected: usize, actual: usize },
    ResultOrdinal { expected: u32, actual: u32 },
    EmptyMemory { ordinal: u32 },
    MissingDiagnostic { ordinal: u32 },
    EmptyDiagnostic { ordinal: u32 },
    EmptyExecutionRef { ordinal: u32 },
}

impl std::fmt::Display for SpawnValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooFewTasks => f.write_str("spine.spawn requires at least two tasks"),
            Self::EmptyTaskSummary { ordinal } => {
                write!(f, "spine.spawn task {ordinal} requires a non-empty summary")
            }
            Self::EmptyTaskPrompt { ordinal } => {
                write!(f, "spine.spawn task {ordinal} requires a non-empty prompt")
            }
            Self::InvalidSchema { schema } => {
                write!(f, "unsupported spine.spawn receipt schema `{schema}`")
            }
            Self::ResultCount { expected, actual } => write!(
                f,
                "spine.spawn receipt has {actual} results; expected {expected}"
            ),
            Self::ResultOrdinal { expected, actual } => write!(
                f,
                "spine.spawn receipt result ordinal {actual}; expected {expected}"
            ),
            Self::EmptyMemory { ordinal } => {
                write!(f, "spine.spawn result {ordinal} requires non-empty memory")
            }
            Self::MissingDiagnostic { ordinal } => write!(
                f,
                "spine.spawn non-completed result {ordinal} requires a diagnostic"
            ),
            Self::EmptyDiagnostic { ordinal } => {
                write!(f, "spine.spawn result {ordinal} has an empty diagnostic")
            }
            Self::EmptyExecutionRef { ordinal } => {
                write!(f, "spine.spawn result {ordinal} has an empty execution_ref")
            }
        }
    }
}

impl std::error::Error for SpawnValidationError {}

impl SpawnReceipt {
    pub fn validate_for(&self, tasks: &[SpawnTask]) -> Result<(), SpawnValidationError> {
        if tasks.len() < 2 {
            return Err(SpawnValidationError::TooFewTasks);
        }
        for (ordinal, task) in tasks.iter().enumerate() {
            if task.summary.trim().is_empty() {
                return Err(SpawnValidationError::EmptyTaskSummary { ordinal });
            }
            if task.prompt.trim().is_empty() {
                return Err(SpawnValidationError::EmptyTaskPrompt { ordinal });
            }
        }
        if self.schema != SPINE_SPAWN_RESULT_SCHEMA {
            return Err(SpawnValidationError::InvalidSchema {
                schema: self.schema.clone(),
            });
        }
        if self.results.len() != tasks.len() {
            return Err(SpawnValidationError::ResultCount {
                expected: tasks.len(),
                actual: self.results.len(),
            });
        }
        for (expected, result) in self.results.iter().enumerate() {
            let expected = u32::try_from(expected).unwrap_or(u32::MAX);
            if result.ordinal != expected {
                return Err(SpawnValidationError::ResultOrdinal {
                    expected,
                    actual: result.ordinal,
                });
            }
            if result.memory_body.trim().is_empty() {
                return Err(SpawnValidationError::EmptyMemory {
                    ordinal: result.ordinal,
                });
            }
            match result.diagnostic.as_deref() {
                None if result.outcome != SpawnOutcome::Completed => {
                    return Err(SpawnValidationError::MissingDiagnostic {
                        ordinal: result.ordinal,
                    });
                }
                Some(diagnostic) if diagnostic.trim().is_empty() => {
                    return Err(SpawnValidationError::EmptyDiagnostic {
                        ordinal: result.ordinal,
                    });
                }
                None | Some(_) => {}
            }
            if result
                .execution_ref
                .as_deref()
                .is_some_and(|execution_ref| execution_ref.trim().is_empty())
            {
                return Err(SpawnValidationError::EmptyExecutionRef {
                    ordinal: result.ordinal,
                });
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolUse {
    pub call_id: String,
    pub name: String,
    pub arguments: String,
    pub outcome: Option<ToolOutcome>,
    pub output: Option<String>,
    #[serde(default)]
    pub output_boundary: Option<RawBoundary>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrimSlice {
    Head {
        head: usize,
    },
    Tail {
        tail: usize,
    },
    Anchor {
        anchor: String,
        preceding: usize,
        following: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrimOperation {
    Snip,
    Slice(TrimSlice),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrimRequest {
    pub trim_id: String,
    pub operation: TrimOperation,
}

impl TrimRequest {
    pub fn parse(arguments: &str) -> Result<Self, String> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Args {
            #[serde(rename = "TRIM_ID")]
            trim_id: String,
            op: String,
            #[serde(default)]
            head: Option<usize>,
            #[serde(default)]
            tail: Option<usize>,
            #[serde(default)]
            anchor: Option<String>,
            #[serde(default)]
            preceding: Option<usize>,
            #[serde(default)]
            following: Option<usize>,
        }

        let args: Args = serde_json::from_str(arguments).map_err(|error| error.to_string())?;
        let trim_id = args.trim_id.trim().to_string();
        if trim_id.is_empty() {
            return Err("spine.trim requires a non-empty TRIM_ID".to_string());
        }
        let operation = match args.op.as_str() {
            "snip"
                if args.head.is_none()
                    && args.tail.is_none()
                    && args.anchor.is_none()
                    && args.preceding.is_none()
                    && args.following.is_none() =>
            {
                TrimOperation::Snip
            }
            "slice" => {
                let shape_count = usize::from(args.head.is_some())
                    + usize::from(args.tail.is_some())
                    + usize::from(args.anchor.is_some());
                if shape_count != 1 {
                    return Err("spine.trim slice requires exactly one slice shape".to_string());
                }
                if let Some(head) = args.head {
                    if args.preceding.is_some() || args.following.is_some() {
                        return Err("head slice cannot include an anchor window".to_string());
                    }
                    TrimOperation::Slice(TrimSlice::Head { head })
                } else if let Some(tail) = args.tail {
                    if args.preceding.is_some() || args.following.is_some() {
                        return Err("tail slice cannot include an anchor window".to_string());
                    }
                    TrimOperation::Slice(TrimSlice::Tail { tail })
                } else {
                    let anchor = args.anchor.unwrap_or_default().trim().to_string();
                    let (Some(preceding), Some(following)) = (args.preceding, args.following)
                    else {
                        return Err("anchor slice requires preceding and following".to_string());
                    };
                    if anchor.is_empty() {
                        return Err("anchor slice requires a non-empty anchor".to_string());
                    }
                    TrimOperation::Slice(TrimSlice::Anchor {
                        anchor,
                        preceding,
                        following,
                    })
                }
            }
            _ => return Err("spine.trim op must be snip or slice".to_string()),
        };
        Ok(Self { trim_id, operation })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrimEdit {
    Tagged { trim_id: String, body: String },
    Snipped,
    Sliced(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TrimProjection {
    pub(crate) edits: std::collections::BTreeMap<RawBoundary, (String, TrimEdit)>,
}

impl TrimProjection {
    pub fn edit(&self, boundary: RawBoundary, call_id: &str) -> Option<&TrimEdit> {
        self.edits
            .get(&boundary)
            .filter(|(expected_call_id, _)| expected_call_id == call_id)
            .map(|(_, edit)| edit)
    }

    pub fn validate(&self, request: &TrimRequest) -> Result<(), String> {
        let Some((_, edit)) = self.edits.values().find(|(_, edit)| {
            matches!(edit, TrimEdit::Tagged { trim_id, .. } if trim_id == &request.trim_id)
        }) else {
            return Err(format!("TRIM_ID {} is expired or not found; do not retry", request.trim_id));
        };
        if let TrimOperation::Slice(slice) = &request.operation {
            let body = match edit {
                TrimEdit::Tagged { body, .. } => body,
                _ => return Err("TRIM_ID has already been trimmed; do not retry".to_string()),
            };
            apply_trim_slice(body, slice)
                .map(|_| ())
                .ok_or_else(|| "trim slice anchor was not found; do not retry".to_string())?;
        }
        Ok(())
    }

    pub fn derive(events: &[RolloutEvent]) -> Self {
        crate::reducer::derive_trim_projection(events)
    }
}

pub(crate) fn apply_trim_slice(text: &str, slice: &TrimSlice) -> Option<String> {
    match slice {
        TrimSlice::Head { head } => Some(text.chars().take(*head).collect()),
        TrimSlice::Tail { tail } => {
            let chars = text.chars().collect::<Vec<_>>();
            Some(chars[chars.len().saturating_sub(*tail)..].iter().collect())
        }
        TrimSlice::Anchor {
            anchor,
            preceding,
            following,
        } => {
            let lines = text.split_inclusive('\n').collect::<Vec<_>>();
            let line = lines.iter().position(|line| line.contains(anchor))?;
            let start = line.saturating_sub(*preceding);
            let end = line.saturating_add(*following + 1).min(lines.len());
            Some(lines[start..end].concat())
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallGroup {
    pub start: RawBoundary,
    pub end: RawBoundary,
    pub leading_assistant_messages: Vec<Message>,
    pub calls: Vec<ToolUse>,
}

impl ToolCallGroup {
    pub fn is_complete(&self) -> bool {
        self.calls
            .iter()
            .all(|call| call.outcome.is_some() && call.output.is_some())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RolloutEvent {
    Message(Message),
    ToolCall(ToolCallGroup),
    Compact {
        boundary: RawBoundary,
        replacement_history: Vec<ContextItem>,
    },
}

impl RolloutEvent {
    pub fn boundary(&self) -> RawBoundary {
        match self {
            Self::Message(message) => message.boundary,
            Self::ToolCall(group) => group.end,
            Self::Compact { boundary, .. } => *boundary,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeKind {
    RootEpoch,
    Task,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeStatus {
    Live,
    Opened,
    Closed,
    Compacted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemorySlot {
    User {
        owner_node: NodeId,
        message: Message,
        anchor: u64,
    },
    Summary {
        owner_node: NodeId,
        source: RawSpan,
        body: String,
    },
    SpawnEvidence {
        owner_node: NodeId,
        source: RawSpan,
        task: SpawnTask,
        outcome: SpawnOutcome,
        diagnostic: Option<String>,
        execution_ref: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NativeItemRef {
    CompactReplacement {
        compact_boundary: RawBoundary,
        index: u32,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContextItem {
    Message {
        message: Message,
        user_anchor: Option<u64>,
    },
    ToolCall(ToolCallGroup),
    SyntheticNode {
        node_id: NodeId,
        summary: String,
        status: NodeStatus,
    },
    MemorySlot(MemorySlot),
    Native {
        source: NativeItemRef,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeSnapshot {
    pub id: NodeId,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    pub kind: NodeKind,
    pub status: NodeStatus,
    pub summary: Option<String>,
    pub memory: Option<Vec<MemorySlot>>,
    pub start: RawBoundary,
    pub end: Option<RawBoundary>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpineProjection {
    pub nodes: Vec<NodeSnapshot>,
    pub cursor: NodeId,
    pub visible_context: Vec<ContextItem>,
    pub last_boundary: Option<RawBoundary>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextEdit {
    pub start: usize,
    pub delete: usize,
    pub insert: Vec<ContextItem>,
}

impl ContextEdit {
    pub fn between(before: &[ContextItem], after: &[ContextItem]) -> Self {
        let common_prefix = before
            .iter()
            .zip(after)
            .take_while(|(left, right)| left == right)
            .count();
        let max_suffix = before
            .len()
            .saturating_sub(common_prefix)
            .min(after.len().saturating_sub(common_prefix));
        let common_suffix = before
            .iter()
            .rev()
            .zip(after.iter().rev())
            .take(max_suffix)
            .take_while(|(left, right)| left == right)
            .count();

        Self {
            start: common_prefix,
            delete: before.len() - common_prefix - common_suffix,
            insert: after[common_prefix..after.len() - common_suffix].to_vec(),
        }
    }

    pub fn apply(&self, context: &mut Vec<ContextItem>) {
        context.splice(
            self.start..self.start + self.delete,
            self.insert.iter().cloned(),
        );
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionDelta {
    pub context_edit: ContextEdit,
    pub projection: SpineProjection,
}
