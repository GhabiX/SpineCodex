use serde::Deserialize;
use serde::Serialize;
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RawBoundary(pub u64);

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolUse {
    pub call_id: String,
    pub name: String,
    pub arguments: String,
    pub outcome: Option<ToolOutcome>,
    pub output: Option<String>,
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
pub enum MemoryPart {
    User {
        anchor: u64,
        content: String,
    },
    Child {
        node_id: NodeId,
        parts: Vec<MemoryPart>,
    },
    Model(String),
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
    Memory {
        node_id: NodeId,
        parts: Vec<MemoryPart>,
    },
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
    pub memory: Option<Vec<MemoryPart>>,
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
