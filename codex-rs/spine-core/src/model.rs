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

    /// Whether this node is `scope` or one of its descendants.
    pub fn is_within(&self, scope: &NodeId) -> bool {
        self.0.starts_with(&scope.0)
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
    /// A host-generated user-role context fragment, not true user evidence.
    ///
    /// It remains user-role when rendered to the model, but the reducer must
    /// not assign a `[U#]` anchor or preserve it as a user-memory slot.
    ContextualUser,
    Assistant,
    /// An assistant message that ended its sampling with `stop` and no tool
    /// call: the agent's reply to the user. The host supplies only its text
    /// blocks as content. It renders as assistant, and a closed node keeps it
    /// as a reference to its source item, as it keeps a user message.
    Reply,
    Developer,
    System,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub boundary: RawBoundary,
    pub role: MessageRole,
    pub content: String,
}

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

/// Join state of a spawned child, derived from the arrivals and namespace
/// continuations the reducer has seen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpawnState {
    /// Born in this namespace; its result has not arrived.
    Pending,
    /// Its result was admitted with this outcome.
    Arrived(SpawnOutcome),
    /// Born in an earlier namespace and unarrived when this one continued it.
    /// Here it is context only: it is joined where it was born.
    Inherited,
}

/// The final result of one spawned child, admitted into its parent as an
/// external input. A completed child carries its memory; any other outcome
/// carries a diagnostic and no memory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpawnArrival {
    pub producer: NodeId,
    pub outcome: SpawnOutcome,
    #[serde(default)]
    pub memory: Option<String>,
    #[serde(default)]
    pub diagnostic: Option<String>,
    #[serde(default)]
    pub execution_ref: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpawnValidationError {
    TooFewTasks,
    TooManyTasks {
        max: usize,
        actual: usize,
    },
    AggregateTooLarge {
        max_bytes: usize,
        actual_bytes: usize,
    },
    FieldTooLarge {
        field: &'static str,
        max_bytes: usize,
    },
    EmptyTaskSummary {
        ordinal: usize,
    },
    EmptyTaskPrompt {
        ordinal: usize,
    },
    ArrivalMemory {
        outcome: SpawnOutcome,
    },
    ArrivalDiagnostic,
    EmptyField(&'static str),
}

impl std::fmt::Display for SpawnValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooFewTasks => f.write_str("spine.spawn requires at least two tasks"),
            Self::TooManyTasks { max, actual } => {
                write!(f, "spine.spawn has {actual} tasks; maximum is {max}")
            }
            Self::AggregateTooLarge {
                max_bytes,
                actual_bytes,
            } => write!(
                f,
                "spine.spawn task payload is {actual_bytes} bytes; maximum is {max_bytes}"
            ),
            Self::FieldTooLarge { field, max_bytes } => {
                write!(f, "spine.spawn field {field} exceeds {max_bytes} bytes")
            }
            Self::EmptyTaskSummary { ordinal } => {
                write!(f, "spine.spawn task {ordinal} requires a non-empty summary")
            }
            Self::EmptyTaskPrompt { ordinal } => {
                write!(f, "spine.spawn task {ordinal} requires a non-empty prompt")
            }
            Self::ArrivalMemory { outcome } => match outcome {
                SpawnOutcome::Completed => {
                    f.write_str("a completed spawn arrival requires non-empty memory")
                }
                SpawnOutcome::Errored | SpawnOutcome::Aborted => {
                    f.write_str("a non-completed spawn arrival must not carry memory")
                }
            },
            Self::ArrivalDiagnostic => {
                f.write_str("a non-completed spawn arrival requires a diagnostic")
            }
            Self::EmptyField(field) => write!(f, "spine.spawn field {field} is empty"),
        }
    }
}

impl std::error::Error for SpawnValidationError {}

pub fn validate_spawn_tasks(tasks: &[SpawnTask]) -> Result<(), SpawnValidationError> {
    if tasks.len() < 2 {
        return Err(SpawnValidationError::TooFewTasks);
    }
    if tasks.len() > crate::MAX_SPAWN_TASKS {
        return Err(SpawnValidationError::TooManyTasks {
            max: crate::MAX_SPAWN_TASKS,
            actual: tasks.len(),
        });
    }
    for (ordinal, task) in tasks.iter().enumerate() {
        if task.summary.trim().is_empty() {
            return Err(SpawnValidationError::EmptyTaskSummary { ordinal });
        }
        if task.prompt.trim().is_empty() {
            return Err(SpawnValidationError::EmptyTaskPrompt { ordinal });
        }
        validate_spawn_field("summary", &task.summary, crate::MAX_SUMMARY_BYTES)?;
        validate_spawn_field("prompt", &task.prompt, crate::MAX_SPAWN_PROMPT_BYTES)?;
    }
    let actual_bytes = tasks.iter().fold(0usize, |total, task| {
        total
            .saturating_add(task.summary.len())
            .saturating_add(task.prompt.len())
    });
    if actual_bytes > crate::MAX_SPAWN_BATCH_BYTES {
        return Err(SpawnValidationError::AggregateTooLarge {
            max_bytes: crate::MAX_SPAWN_BATCH_BYTES,
            actual_bytes,
        });
    }
    Ok(())
}

impl SpawnArrival {
    pub fn validate(&self) -> Result<(), SpawnValidationError> {
        match (&self.outcome, self.memory.as_deref()) {
            (SpawnOutcome::Completed, Some(memory)) if !memory.trim().is_empty() => {
                validate_spawn_field("memory", memory, crate::MAX_MEMORY_BYTES)?;
            }
            (SpawnOutcome::Errored | SpawnOutcome::Aborted, None) => {}
            (outcome, _) => {
                return Err(SpawnValidationError::ArrivalMemory { outcome: *outcome });
            }
        }
        match self.diagnostic.as_deref() {
            None if self.outcome != SpawnOutcome::Completed => {
                return Err(SpawnValidationError::ArrivalDiagnostic);
            }
            Some(diagnostic) if diagnostic.trim().is_empty() => {
                return Err(SpawnValidationError::EmptyField("diagnostic"));
            }
            Some(diagnostic) => {
                validate_spawn_field("diagnostic", diagnostic, crate::MAX_SUMMARY_BYTES)?;
            }
            None => {}
        }
        match self.execution_ref.as_deref() {
            Some(execution_ref) if execution_ref.trim().is_empty() => {
                Err(SpawnValidationError::EmptyField("execution_ref"))
            }
            Some(execution_ref) => {
                validate_spawn_field("execution_ref", execution_ref, crate::MAX_SUMMARY_BYTES)
            }
            None => Ok(()),
        }
    }

    pub(crate) fn retained_bytes(&self) -> usize {
        [&self.memory, &self.diagnostic, &self.execution_ref]
            .into_iter()
            .flatten()
            .fold(0usize, |total, value| total.saturating_add(value.len()))
    }
}

fn validate_spawn_field(
    field: &'static str,
    value: &str,
    max_bytes: usize,
) -> Result<(), SpawnValidationError> {
    if value.len() > max_bytes {
        return Err(SpawnValidationError::FieldTooLarge { field, max_bytes });
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum RolloutEvent {
    Message(Message),
    SourceSpan {
        span: RawSpan,
        retained_bytes: usize,
    },
    Opaque {
        boundary: RawBoundary,
    },
    Synthetic {
        boundary: RawBoundary,
        item: ContextItem,
    },
    Arrival {
        boundary: RawBoundary,
        arrival: SpawnArrival,
    },
    Compact {
        boundary: RawBoundary,
        replacement_history: Vec<ContextItem>,
    },
}

impl RolloutEvent {
    pub fn boundary(&self) -> RawBoundary {
        match self {
            Self::Message(message) => message.boundary,
            Self::SourceSpan { span, .. } => span.end,
            Self::Opaque { boundary }
            | Self::Synthetic { boundary, .. }
            | Self::Arrival { boundary, .. }
            | Self::Compact { boundary, .. } => *boundary,
        }
    }

    pub(crate) fn retained_bytes(&self) -> usize {
        match self {
            Self::Message(message) => message.content.len(),
            Self::SourceSpan { retained_bytes, .. } => *retained_bytes,
            Self::Opaque { .. } => 0,
            Self::Synthetic { item, .. } => item.retained_synthetic_bytes(),
            Self::Arrival { arrival, .. } => arrival.retained_bytes(),
            Self::Compact {
                replacement_history,
                ..
            } => replacement_history
                .iter()
                .map(ContextItem::retained_synthetic_bytes)
                .fold(0usize, usize::saturating_add),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeKind {
    RootEpoch,
    Task,
    /// The immutable birth record of a spawned child. Whether the child has
    /// finished is derived from its arrival, not stored on the node.
    Spawn,
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
    /// A spawned child's birth record: the task it was given.
    Birth {
        owner_node: NodeId,
        source: RawSpan,
        task: SpawnTask,
    },
    /// A spawned child's final result, kept where it was admitted.
    Arrival {
        owner_node: NodeId,
        boundary: RawBoundary,
        arrival: SpawnArrival,
    },
    /// The agent's reply to the user, kept from a closed node. Like a user
    /// slot it renders its source item, labelled `ContextLabel::Reply`.
    Reply {
        owner_node: NodeId,
        message: Message,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NativeItemRef {
    /// A source item in the persisted native rollout, identified by the
    /// stable source ordinal assigned by the host adapter.
    Rollout { ordinal: RawBoundary },
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
    SourceSpan {
        span: RawSpan,
    },
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

impl ContextItem {
    pub(crate) fn retained_synthetic_bytes(&self) -> usize {
        match self {
            Self::Message { message, .. } if message.role == MessageRole::ContextualUser => {
                message.content.len()
            }
            Self::SyntheticNode { summary, .. } => summary.len(),
            Self::MemorySlot(MemorySlot::Summary { body, .. }) => body.len(),
            Self::MemorySlot(MemorySlot::Birth { task, .. }) => {
                task.summary.len().saturating_add(task.prompt.len())
            }
            Self::Message { .. }
            | Self::SourceSpan { .. }
            | Self::MemorySlot(
                MemorySlot::User { .. } | MemorySlot::Arrival { .. } | MemorySlot::Reply { .. },
            )
            | Self::Native { .. } => 0,
        }
    }
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
    /// For a `Spawn` node, its join state; `None` for other kinds.
    pub spawn: Option<SpawnState>,
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
