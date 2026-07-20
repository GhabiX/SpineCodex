use crate::ContextEdit;
use crate::ContextItem;
use crate::MemorySlot;
use crate::Message;
use crate::MessageRole;
use crate::NodeId;
use crate::NodeKind;
use crate::NodeSnapshot;
use crate::NodeStatus;
use crate::ProjectionDelta;
use crate::RawBoundary;
use crate::RawSpan;
use crate::RolloutEvent;
use crate::SpawnReceipt;
use crate::SpawnTask;
use crate::SpineProjection;
use crate::ToolCallGroup;
use crate::ToolOutcome;
use crate::TrimEdit;
use crate::TrimOperation;
use crate::TrimProjection;
use crate::TrimRequest;
use serde::Deserialize;

const SPINE_OPEN: &str = "spine.open";
const SPINE_CLOSE: &str = "spine.close";
const SPINE_NEXT: &str = "spine.next";
const SPINE_SPAWN: &str = "spine.spawn";
const SPINE_TRIM: &str = "spine.trim";
pub const TOOL_RESPONSE_TRIM_THRESHOLD_BYTES: usize = 10_000;

#[derive(Clone, Debug, PartialEq, Eq)]
enum NodeEntry {
    Leaf(ContextItem),
    Child(NodeId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RuntimeNode {
    id: NodeId,
    parent: Option<NodeId>,
    children: Vec<NodeId>,
    kind: NodeKind,
    status: NodeStatus,
    summary: Option<String>,
    memory: Option<Vec<MemorySlot>>,
    start: RawBoundary,
    end: Option<RawBoundary>,
    baseline: Vec<ContextItem>,
    entries: Vec<NodeEntry>,
}

impl RuntimeNode {
    fn snapshot(&self) -> NodeSnapshot {
        NodeSnapshot {
            id: self.id.clone(),
            parent: self.parent.clone(),
            children: self.children.clone(),
            kind: self.kind,
            status: self.status,
            summary: self.summary.clone(),
            memory: self.memory.clone(),
            start: self.start,
            end: self.end,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpineReducer {
    nodes: Vec<RuntimeNode>,
    root_epochs: Vec<NodeId>,
    cursor: NodeId,
    next_user_anchor: u64,
    last_boundary: Option<RawBoundary>,
}

impl Default for SpineReducer {
    fn default() -> Self {
        Self::new()
    }
}

impl SpineReducer {
    pub fn new() -> Self {
        let root_id = NodeId::root_epoch(1);
        Self {
            nodes: vec![RuntimeNode {
                id: root_id.clone(),
                parent: None,
                children: Vec::new(),
                kind: NodeKind::RootEpoch,
                status: NodeStatus::Live,
                summary: Some("root".to_string()),
                memory: None,
                start: RawBoundary(0),
                end: None,
                baseline: Vec::new(),
                entries: Vec::new(),
            }],
            root_epochs: vec![root_id.clone()],
            cursor: root_id,
            next_user_anchor: 1,
            last_boundary: None,
        }
    }

    pub fn derive(events: &[RolloutEvent]) -> SpineProjection {
        let mut reducer = Self::new();
        for event in events {
            reducer.apply(event.clone());
        }
        reducer.projection()
    }

    pub fn apply(&mut self, event: RolloutEvent) -> ProjectionDelta {
        let before = self.projection().visible_context;
        self.last_boundary = Some(event.boundary());
        match event {
            RolloutEvent::Message(message) => self.apply_message(message),
            RolloutEvent::ToolCall(group) => self.apply_toolcall(group),
            RolloutEvent::Compact {
                boundary,
                replacement_history,
            } => self.apply_compact(boundary, replacement_history),
        }
        let projection = self.projection();
        ProjectionDelta {
            context_edit: ContextEdit::between(&before, &projection.visible_context),
            projection,
        }
    }

    pub fn projection(&self) -> SpineProjection {
        SpineProjection {
            nodes: self.nodes.iter().map(RuntimeNode::snapshot).collect(),
            cursor: self.cursor.clone(),
            visible_context: self.render_current_epoch(),
            last_boundary: self.last_boundary,
        }
    }

    fn apply_message(&mut self, message: Message) {
        let user_anchor = (message.role == MessageRole::User).then(|| {
            let anchor = self.next_user_anchor;
            self.next_user_anchor += 1;
            anchor
        });
        self.push_cursor_entry(NodeEntry::Leaf(ContextItem::Message {
            message,
            user_anchor,
        }));
    }

    fn apply_toolcall(&mut self, group: ToolCallGroup) {
        let control = classify_control(&group);
        match control {
            Some(Control::Open { summary }) => self.open(group, summary),
            Some(Control::Close { memory }) if self.cursor_kind() == NodeKind::Task => {
                self.close(group, memory)
            }
            Some(Control::Next { summary, memory }) if self.cursor_kind() == NodeKind::Task => {
                self.next(group, summary, memory)
            }
            Some(Control::Spawn { calls }) => self.spawn(group, calls),
            _ => self.push_cursor_entry(NodeEntry::Leaf(ContextItem::ToolCall(group))),
        }
    }

    fn open(&mut self, group: ToolCallGroup, summary: String) {
        let parent_id = self.cursor.clone();
        let parent_index = self.node_index(&parent_id);
        let child_ordinal = self.nodes[parent_index].children.len() as u32 + 1;
        let child_id = parent_id.child(child_ordinal);
        self.nodes[parent_index].children.push(child_id.clone());
        self.nodes[parent_index]
            .entries
            .push(NodeEntry::Child(child_id.clone()));
        self.nodes[parent_index].status = NodeStatus::Opened;
        self.nodes.push(RuntimeNode {
            id: child_id.clone(),
            parent: Some(parent_id),
            children: Vec::new(),
            kind: NodeKind::Task,
            status: NodeStatus::Live,
            summary: Some(summary),
            memory: None,
            start: group.start,
            end: None,
            baseline: Vec::new(),
            entries: vec![NodeEntry::Leaf(ContextItem::ToolCall(group))],
        });
        self.cursor = child_id;
    }

    fn close(&mut self, group: ToolCallGroup, model_memory: String) {
        let closed_id = self.cursor.clone();
        let closed_index = self.node_index(&closed_id);
        let parent_id = self.nodes[closed_index]
            .parent
            .clone()
            .expect("task node has a parent");
        let memory = self.assemble_memory(
            closed_index,
            model_memory,
            RawSpan {
                start: group.start,
                end: group.end,
            },
        );
        self.nodes[closed_index].memory = Some(memory);
        self.nodes[closed_index].status = NodeStatus::Closed;
        self.nodes[closed_index].end = Some(group.start);
        let parent_index = self.node_index(&parent_id);
        self.nodes[parent_index].status = NodeStatus::Live;
        self.nodes[parent_index]
            .entries
            .push(NodeEntry::Leaf(ContextItem::ToolCall(group)));
        self.cursor = parent_id;
    }

    fn next(&mut self, group: ToolCallGroup, summary: String, model_memory: String) {
        let closed_id = self.cursor.clone();
        let closed_index = self.node_index(&closed_id);
        let parent_id = self.nodes[closed_index]
            .parent
            .clone()
            .expect("task node has a parent");
        let memory = self.assemble_memory(
            closed_index,
            model_memory,
            RawSpan {
                start: group.start,
                end: group.end,
            },
        );
        self.nodes[closed_index].memory = Some(memory);
        self.nodes[closed_index].status = NodeStatus::Closed;
        self.nodes[closed_index].end = Some(group.start);

        let parent_index = self.node_index(&parent_id);
        let child_ordinal = self.nodes[parent_index].children.len() as u32 + 1;
        let sibling_id = parent_id.child(child_ordinal);
        self.nodes[parent_index].children.push(sibling_id.clone());
        self.nodes[parent_index]
            .entries
            .push(NodeEntry::Child(sibling_id.clone()));
        self.nodes[parent_index].status = NodeStatus::Opened;
        self.nodes.push(RuntimeNode {
            id: sibling_id.clone(),
            parent: Some(parent_id),
            children: Vec::new(),
            kind: NodeKind::Task,
            status: NodeStatus::Live,
            summary: Some(summary),
            memory: None,
            start: group.start,
            end: None,
            baseline: Vec::new(),
            entries: vec![NodeEntry::Leaf(ContextItem::ToolCall(group))],
        });
        self.cursor = sibling_id;
    }

    fn spawn(&mut self, group: ToolCallGroup, calls: Vec<ValidSpawnCall>) {
        let parent_id = self.cursor.clone();
        let parent_index = self.node_index(&parent_id);
        let first_child_ordinal = self.nodes[parent_index].children.len() as u32 + 1;
        let source = RawSpan {
            start: group.start,
            end: group.end,
        };
        let child_count = calls.iter().map(|call| call.tasks.len()).sum();
        let mut child_ids = Vec::with_capacity(child_count);
        let mut children = Vec::with_capacity(child_count);
        let mut child_offset = 0usize;
        for call in calls {
            debug_assert!(call.call_ordinal < group.calls.len());
            for (task, result) in call.tasks.into_iter().zip(call.receipt.results) {
                let offset = u32::try_from(child_offset).unwrap_or(u32::MAX);
                let child_id = parent_id.child(first_child_ordinal.saturating_add(offset));
                let memory = vec![
                    MemorySlot::SpawnEvidence {
                        owner_node: child_id.clone(),
                        source,
                        task: task.clone(),
                        outcome: result.outcome,
                        diagnostic: result.diagnostic,
                        execution_ref: result.execution_ref,
                    },
                    MemorySlot::Summary {
                        owner_node: child_id.clone(),
                        source,
                        body: result.memory_body,
                    },
                ];
                child_ids.push(child_id.clone());
                children.push(RuntimeNode {
                    id: child_id,
                    parent: Some(parent_id.clone()),
                    children: Vec::new(),
                    kind: NodeKind::Task,
                    status: NodeStatus::Closed,
                    summary: Some(task.summary),
                    memory: Some(memory),
                    start: group.start,
                    end: Some(group.end),
                    baseline: Vec::new(),
                    entries: Vec::new(),
                });
                child_offset = child_offset.saturating_add(1);
            }
        }
        self.nodes[parent_index]
            .entries
            .push(NodeEntry::Leaf(ContextItem::ToolCall(group)));
        self.nodes[parent_index]
            .children
            .extend(child_ids.iter().cloned());
        self.nodes[parent_index]
            .entries
            .extend(child_ids.into_iter().map(NodeEntry::Child));
        self.nodes.extend(children);
    }

    fn apply_compact(&mut self, boundary: RawBoundary, replacement_history: Vec<ContextItem>) {
        let current_epoch = self
            .root_epochs
            .last()
            .cloned()
            .expect("a reducer always has a root epoch");
        for node in &mut self.nodes {
            if node.id.parts().first() == current_epoch.parts().first()
                && node.status != NodeStatus::Closed
            {
                node.status = NodeStatus::Compacted;
                node.end.get_or_insert(boundary);
            }
        }

        let next_epoch = self.root_epochs.len() as u32 + 1;
        let next_id = NodeId::root_epoch(next_epoch);
        self.nodes.push(RuntimeNode {
            id: next_id.clone(),
            parent: None,
            children: Vec::new(),
            kind: NodeKind::RootEpoch,
            status: NodeStatus::Live,
            summary: Some("root".to_string()),
            memory: None,
            start: boundary,
            end: None,
            baseline: replacement_history,
            entries: Vec::new(),
        });
        self.root_epochs.push(next_id.clone());
        self.cursor = next_id;
    }

    fn assemble_memory(
        &self,
        node_index: usize,
        model_memory: String,
        source: RawSpan,
    ) -> Vec<MemorySlot> {
        let owner_node = self.nodes[node_index].id.clone();
        let mut slots = Vec::new();
        for entry in &self.nodes[node_index].entries {
            match entry {
                NodeEntry::Leaf(ContextItem::Message {
                    message,
                    user_anchor: Some(anchor),
                }) if message.role == MessageRole::User => slots.push(MemorySlot::User {
                    owner_node: owner_node.clone(),
                    message: message.clone(),
                    anchor: *anchor,
                }),
                NodeEntry::Child(child_id) => {
                    let child = &self.nodes[self.node_index(child_id)];
                    if let Some(memory) = &child.memory {
                        slots.extend(memory.iter().cloned());
                    }
                }
                _ => {}
            }
        }
        slots.push(MemorySlot::Summary {
            owner_node,
            source,
            body: model_memory,
        });
        slots
    }

    fn render_current_epoch(&self) -> Vec<ContextItem> {
        let root_id = self
            .root_epochs
            .last()
            .expect("a reducer always has a root epoch");
        let root = &self.nodes[self.node_index(root_id)];
        let mut context = root.baseline.clone();
        self.render_entries(&root.entries, &mut context);
        context
    }

    fn render_node(&self, node_id: &NodeId, context: &mut Vec<ContextItem>) {
        let node = &self.nodes[self.node_index(node_id)];
        match node.status {
            NodeStatus::Closed => context.extend(
                node.memory
                    .iter()
                    .flatten()
                    .cloned()
                    .map(ContextItem::MemorySlot),
            ),
            NodeStatus::Live | NodeStatus::Opened => {
                context.push(ContextItem::SyntheticNode {
                    node_id: node.id.clone(),
                    summary: node.summary.clone().unwrap_or_default(),
                    status: node.status,
                });
                self.render_entries(&node.entries, context);
            }
            NodeStatus::Compacted => {}
        }
    }

    fn render_entries(&self, entries: &[NodeEntry], context: &mut Vec<ContextItem>) {
        for entry in entries {
            match entry {
                NodeEntry::Leaf(item) => context.push(item.clone()),
                NodeEntry::Child(node_id) => self.render_node(node_id, context),
            }
        }
    }

    fn push_cursor_entry(&mut self, entry: NodeEntry) {
        let index = self.node_index(&self.cursor);
        self.nodes[index].entries.push(entry);
    }

    fn cursor_kind(&self) -> NodeKind {
        self.nodes[self.node_index(&self.cursor)].kind
    }

    fn node_index(&self, id: &NodeId) -> usize {
        self.nodes
            .iter()
            .position(|node| &node.id == id)
            .unwrap_or_else(|| panic!("missing runtime node {id}"))
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

enum Control {
    Open { summary: String },
    Close { memory: String },
    Next { summary: String, memory: String },
    Spawn { calls: Vec<ValidSpawnCall> },
}

#[derive(Debug)]
struct ValidSpawnCall {
    call_ordinal: usize,
    tasks: Vec<SpawnTask>,
    receipt: SpawnReceipt,
}

fn classify_control(group: &ToolCallGroup) -> Option<Control> {
    if !group.is_complete() {
        return None;
    }
    if let Some(control) = classify_spawn(group) {
        return Some(control);
    }
    let mut controls = group.calls.iter().filter_map(|call| {
        if call.outcome != Some(ToolOutcome::Succeeded) {
            return None;
        }
        match call.name.as_str() {
            SPINE_OPEN => serde_json::from_str::<OpenArgs>(&call.arguments)
                .ok()
                .and_then(|args| non_empty(args.summary))
                .map(|summary| Control::Open { summary }),
            SPINE_CLOSE => serde_json::from_str::<CloseArgs>(&call.arguments)
                .ok()
                .and_then(|args| non_empty(args.memory))
                .map(|memory| Control::Close { memory }),
            SPINE_NEXT => serde_json::from_str::<NextArgs>(&call.arguments)
                .ok()
                .and_then(|args| Some((non_empty(args.summary)?, non_empty(args.memory)?)))
                .map(|(summary, memory)| Control::Next { summary, memory }),
            _ => None,
        }
    });
    let control = controls.next()?;
    controls.next().is_none().then_some(control)
}

fn classify_spawn(group: &ToolCallGroup) -> Option<Control> {
    if !group.calls.iter().any(|call| call.name == SPINE_SPAWN) {
        return None;
    }

    if group
        .calls
        .iter()
        .any(|call| matches!(call.name.as_str(), SPINE_OPEN | SPINE_CLOSE | SPINE_NEXT))
    {
        return Some(Control::Spawn { calls: Vec::new() });
    }

    let calls = group
        .calls
        .iter()
        .enumerate()
        .filter_map(|(call_ordinal, call)| {
            if call.name != SPINE_SPAWN || call.outcome != Some(ToolOutcome::Succeeded) {
                return None;
            }
            let tasks = serde_json::from_str::<SpawnArgs>(&call.arguments)
                .ok()?
                .tasks;
            let receipt = serde_json::from_str::<SpawnReceipt>(call.output.as_deref()?).ok()?;
            receipt.validate_for(&tasks).ok()?;
            Some(ValidSpawnCall {
                call_ordinal,
                tasks,
                receipt,
            })
        })
        .collect();
    Some(Control::Spawn { calls })
}

fn non_empty(value: String) -> Option<String> {
    let value = value.trim().to_string();
    (!value.is_empty()).then_some(value)
}

pub(crate) fn derive_trim_projection(events: &[RolloutEvent]) -> TrimProjection {
    let mut projection = TrimProjection::default();
    let mut active = Vec::new();
    for event in events {
        let RolloutEvent::ToolCall(group) = event else {
            continue;
        };
        for call in group
            .calls
            .iter()
            .filter(|call| call.name == SPINE_TRIM && call.outcome == Some(ToolOutcome::Succeeded))
        {
            let Ok(request) = TrimRequest::parse(&call.arguments) else {
                continue;
            };
            apply_trim_request(&mut projection, &active, &request);
        }
        expire_trim_candidates(&mut projection, &mut active);
        for call in group
            .calls
            .iter()
            .filter(|call| !call.name.starts_with("spine."))
        {
            let (Some(boundary), Some(body)) = (call.output_boundary, call.output.as_deref())
            else {
                continue;
            };
            if body.len() <= TOOL_RESPONSE_TRIM_THRESHOLD_BYTES {
                continue;
            }
            let trim_id = format!("trim_{}", boundary.0);
            projection.edits.insert(
                boundary,
                (
                    call.call_id.clone(),
                    TrimEdit::Tagged {
                        trim_id,
                        body: body.to_string(),
                    },
                ),
            );
            active.push(boundary);
        }
    }
    projection
}

fn expire_trim_candidates(projection: &mut TrimProjection, active: &mut Vec<RawBoundary>) {
    for boundary in active.drain(..) {
        if projection
            .edits
            .get(&boundary)
            .is_some_and(|(_, edit)| matches!(edit, TrimEdit::Tagged { .. }))
        {
            projection.edits.remove(&boundary);
        }
    }
}

fn apply_trim_request(
    projection: &mut TrimProjection,
    active: &[RawBoundary],
    request: &TrimRequest,
) {
    let Some(boundary) = active.iter().copied().find(|boundary| {
        projection.edits.get(boundary).is_some_and(|(_, edit)| {
            matches!(edit, TrimEdit::Tagged { trim_id, .. } if trim_id == &request.trim_id)
        })
    }) else {
        return;
    };
    let Some((_, edit)) = projection.edits.get_mut(&boundary) else {
        return;
    };
    match &request.operation {
        TrimOperation::Snip => *edit = TrimEdit::Snipped,
        TrimOperation::Slice(slice) => {
            let body = match edit {
                TrimEdit::Tagged { body, .. } | TrimEdit::Sliced(body) => body.as_str(),
                TrimEdit::Snipped => return,
            };
            let Some(value) = crate::model::apply_trim_slice(body, slice) else {
                return;
            };
            *edit = TrimEdit::Sliced(value);
        }
    }
}
