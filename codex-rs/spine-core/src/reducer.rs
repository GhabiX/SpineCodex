use crate::ContextEdit;
use crate::ContextItem;
use crate::ContextWindowSample;
use crate::ExecutedSpineFact;
use crate::MemorySlot;
use crate::Message;
use crate::MessageRole;
use crate::NodeContextCost;
use crate::NodeId;
use crate::NodeKind;
use crate::NodeSnapshot;
use crate::NodeStatus;
use crate::ProjectionDelta;
use crate::RawBoundary;
use crate::RawSpan;
use crate::RolloutEvent;
use crate::SpawnResult;
use crate::SpawnTask;
use crate::SpineOperationFact;
use crate::SpineProjection;
use crate::ToolOutcome;
use crate::TrimEdit;
use crate::TrimOperation;
use crate::TrimProjection;
use crate::TrimRequest;
use crate::context_char::CompletedCalls;
const SPINE_TRIM: &str = "spine.trim";
pub const TOOL_RESPONSE_TRIM_THRESHOLD_BYTES: usize = 10_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TrimReducer {
    projection: TrimProjection,
    active: Vec<RawBoundary>,
    threshold_bytes: usize,
}

impl TrimReducer {
    pub(crate) fn new(threshold_bytes: usize) -> Self {
        Self {
            projection: TrimProjection::default(),
            active: Vec::new(),
            threshold_bytes,
        }
    }

    pub(crate) fn apply(&mut self, event: &RolloutEvent) {
        match event {
            RolloutEvent::Compact { .. } => {
                self.projection = TrimProjection::default();
                self.active.clear();
            }
            RolloutEvent::Message(_)
            | RolloutEvent::SourceSpan { .. }
            | RolloutEvent::Opaque { .. }
            | RolloutEvent::Synthetic { .. } => {}
        }
    }

    pub(crate) fn apply_completed_calls(&mut self, completed: &CompletedCalls) {
        for call in completed
            .calls
            .iter()
            .filter(|call| call.name == SPINE_TRIM && call.outcome == Some(ToolOutcome::Succeeded))
        {
            let Ok(request) = TrimRequest::parse(&call.arguments) else {
                continue;
            };
            apply_trim_request(&mut self.projection, &self.active, &request);
        }
        expire_trim_candidates(&mut self.projection, &mut self.active);
        self.observe_trim_candidates(completed);
    }

    pub(crate) fn apply_sampling(
        &mut self,
        completed: &[CompletedCalls],
        trims: &[(RawBoundary, &ExecutedSpineFact)],
    ) -> Result<(), TypedTransitionError> {
        for (boundary, fact) in trims {
            let SpineOperationFact::Trim {
                target,
                validated_edit,
                ..
            } = &fact.operation
            else {
                return Err(TypedTransitionError::NonTrimFactInTrimSet);
            };
            if !self.active.contains(boundary) {
                return Err(TypedTransitionError::InactiveTrimTarget(*boundary));
            }
            let Some((call_id, edit)) = self.projection.edits.get_mut(boundary) else {
                return Err(TypedTransitionError::InactiveTrimTarget(*boundary));
            };
            if call_id != &target.call_id {
                return Err(TypedTransitionError::TrimTargetMismatch);
            }
            *edit = validated_edit.clone();
        }

        expire_trim_candidates(&mut self.projection, &mut self.active);
        for completed in completed {
            self.observe_trim_candidates(completed);
        }
        Ok(())
    }

    fn observe_trim_candidates(&mut self, completed: &CompletedCalls) {
        for call in completed
            .calls
            .iter()
            .filter(|call| !call.name.starts_with("spine."))
        {
            let (Some(boundary), Some(body)) = (call.output_boundary, call.output.as_deref())
            else {
                continue;
            };
            if body.len() <= self.threshold_bytes {
                continue;
            }
            let trim_id = format!("trim_{}", boundary.0);
            self.projection.edits.insert(
                boundary,
                (
                    call.call_id.clone(),
                    TrimEdit::Tagged {
                        trim_id,
                        body: body.to_string(),
                        eligible: true,
                    },
                ),
            );
            self.active.push(boundary);
        }
    }

    pub(crate) fn projection(&self) -> &TrimProjection {
        &self.projection
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum NodeEntry {
    Leaf(ContextItem),
    Child(NodeId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RuntimeNode {
    id: NodeId,
    parent: Option<NodeId>,
    status: NodeStatus,
    summary: Option<String>,
    memory: Option<Vec<MemorySlot>>,
    start: RawBoundary,
    end: Option<RawBoundary>,
    open_input_tokens: Option<u64>,
    entries: Vec<NodeEntry>,
}

impl RuntimeNode {
    fn children(&self) -> impl Iterator<Item = &NodeId> {
        self.entries.iter().filter_map(|entry| match entry {
            NodeEntry::Child(child) => Some(child),
            NodeEntry::Leaf(_) => None,
        })
    }

    fn snapshot(&self) -> NodeSnapshot {
        NodeSnapshot {
            id: self.id.clone(),
            parent: self.parent.clone(),
            children: self.children().cloned().collect(),
            kind: if self.parent.is_none() {
                NodeKind::RootEpoch
            } else {
                NodeKind::Task
            },
            status: self.status,
            summary: self.summary.clone(),
            memory: self.memory.clone(),
            start: self.start,
            end: self.end,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpineReducer {
    nodes: Vec<RuntimeNode>,
    current_root: NodeId,
    cursor: NodeId,
    baseline: Vec<ContextItem>,
    next_user_anchor: u64,
    last_boundary: Option<RawBoundary>,
    settled_spawn_call_ids: Vec<String>,
}

impl Default for SpineReducer {
    fn default() -> Self {
        Self::new()
    }
}

impl SpineReducer {
    pub(crate) fn new() -> Self {
        let root_id = NodeId::root_epoch(1);
        Self {
            nodes: vec![RuntimeNode {
                id: root_id.clone(),
                parent: None,
                status: NodeStatus::Live,
                summary: Some("root".to_string()),
                memory: None,
                start: RawBoundary(0),
                end: None,
                open_input_tokens: None,
                entries: Vec::new(),
            }],
            current_root: root_id.clone(),
            cursor: root_id,
            baseline: Vec::new(),
            next_user_anchor: 1,
            last_boundary: None,
            settled_spawn_call_ids: Vec::new(),
        }
    }

    pub(crate) fn apply(&mut self, event: RolloutEvent) -> ProjectionDelta {
        let before = self.render_current_epoch();
        self.last_boundary = Some(event.boundary());
        self.settled_spawn_call_ids.clear();
        match event {
            RolloutEvent::Message(message) => self.apply_message(message),
            RolloutEvent::SourceSpan { span, .. } => self.push_source_span(span),
            RolloutEvent::Opaque { boundary } => {
                self.push_cursor_entry(NodeEntry::Leaf(ContextItem::Native {
                    source: crate::NativeItemRef::Rollout { ordinal: boundary },
                }));
            }
            RolloutEvent::Synthetic { item, .. } => {
                self.push_cursor_entry(NodeEntry::Leaf(item));
            }
            RolloutEvent::Compact {
                boundary,
                replacement_history,
            } => self.apply_compact(boundary, replacement_history),
        }
        self.delta_from(before)
    }

    pub(crate) fn apply_sampling(
        &mut self,
        span: RawSpan,
        facts: &[&ExecutedSpineFact],
        settled_spawn_call_ids: &[String],
        open_input_tokens: Option<u64>,
    ) -> Result<ProjectionDelta, TypedTransitionError> {
        let before = self.render_current_epoch();
        self.last_boundary = Some(span.end);
        self.settled_spawn_call_ids = settled_spawn_call_ids.to_vec();

        let structural = facts
            .iter()
            .copied()
            .filter(|fact| {
                matches!(
                    fact.operation,
                    SpineOperationFact::Open { .. }
                        | SpineOperationFact::Close { .. }
                        | SpineOperationFact::Next { .. }
                        | SpineOperationFact::Spawn { .. }
                )
            })
            .collect::<Vec<_>>();
        let all_spawn = structural
            .iter()
            .all(|fact| matches!(fact.operation, SpineOperationFact::Spawn { .. }));
        if structural.len() > 1 && !all_spawn {
            return Err(TypedTransitionError::MultipleStructuralFacts);
        }

        if structural.len() > 1 {
            let batches = structural
                .iter()
                .filter_map(|fact| match &fact.operation {
                    SpineOperationFact::Spawn {
                        tasks,
                        terminal_results,
                    } => Some((tasks.clone(), terminal_results.clone())),
                    SpineOperationFact::Open { .. }
                    | SpineOperationFact::Close { .. }
                    | SpineOperationFact::Next { .. }
                    | SpineOperationFact::Trim { .. } => None,
                })
                .collect();
            self.spawn(span, batches);
        } else {
            match structural.first().map(|fact| &fact.operation) {
                Some(SpineOperationFact::Open { summary }) => {
                    self.open(span, summary.clone(), open_input_tokens);
                }
                Some(SpineOperationFact::Close { memory }) => {
                    if self.cursor_kind() != NodeKind::Task {
                        return Err(TypedTransitionError::TaskCursorRequired("close"));
                    }
                    self.close(span, memory.clone());
                }
                Some(SpineOperationFact::Next {
                    closed_memory,
                    next_summary,
                }) => {
                    if self.cursor_kind() != NodeKind::Task {
                        return Err(TypedTransitionError::TaskCursorRequired("next"));
                    }
                    self.next(
                        span,
                        next_summary.clone(),
                        closed_memory.clone(),
                        open_input_tokens,
                    );
                }
                Some(SpineOperationFact::Spawn {
                    tasks,
                    terminal_results,
                }) => {
                    self.spawn(span, vec![(tasks.clone(), terminal_results.clone())]);
                }
                Some(SpineOperationFact::Trim { .. }) | None => {
                    self.push_source_span(span);
                }
            }
        }

        Ok(self.delta_from(before))
    }

    pub(crate) fn projection(&self) -> SpineProjection {
        SpineProjection {
            nodes: self.nodes.iter().map(RuntimeNode::snapshot).collect(),
            cursor: self.cursor.clone(),
            visible_context: self.render_current_epoch(),
            last_boundary: self.last_boundary,
            settled_spawn_call_ids: self.settled_spawn_call_ids.clone(),
        }
    }

    fn delta_from(&self, before: Vec<ContextItem>) -> ProjectionDelta {
        let projection = self.projection();
        ProjectionDelta {
            context_edit: ContextEdit::between(&before, &projection.visible_context),
            projection,
        }
    }

    pub(crate) fn node_context_costs(
        &self,
        context_window_samples: &[ContextWindowSample],
    ) -> std::collections::BTreeMap<NodeId, NodeContextCost> {
        self.nodes
            .iter()
            .filter(|node| matches!(node.status, NodeStatus::Live | NodeStatus::Opened))
            .map(|node| {
                let cost = context_window_samples
                    .iter()
                    .find(|sample| sample.boundary.0 > node.start.0)
                    .map_or(NodeContextCost::Unavailable, |sample| {
                        crate::status::context_cost(
                            node.open_input_tokens,
                            sample.model_context_window,
                        )
                    });
                (node.id.clone(), cost)
            })
            .collect()
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

    fn push_source_span(&mut self, span: RawSpan) {
        self.push_cursor_entry(NodeEntry::Leaf(ContextItem::SourceSpan { span }));
    }

    fn open(&mut self, span: RawSpan, summary: String, open_input_tokens: Option<u64>) {
        let parent_id = self.cursor.clone();
        let parent_index = self.node_index(&parent_id);
        let child_ordinal = self.nodes[parent_index].children().count() as u32 + 1;
        let child_id = parent_id.child(child_ordinal);
        self.nodes[parent_index]
            .entries
            .push(NodeEntry::Child(child_id.clone()));
        self.nodes[parent_index].status = NodeStatus::Opened;
        self.nodes.push(RuntimeNode {
            id: child_id.clone(),
            parent: Some(parent_id),
            status: NodeStatus::Live,
            summary: Some(summary),
            memory: None,
            start: span.start,
            end: None,
            open_input_tokens,
            entries: vec![NodeEntry::Leaf(ContextItem::SourceSpan { span })],
        });
        self.cursor = child_id;
    }

    fn close(&mut self, span: RawSpan, model_memory: String) {
        let closed_id = self.cursor.clone();
        let closed_index = self.node_index(&closed_id);
        let parent_id = self.nodes[closed_index]
            .parent
            .clone()
            .expect("task node has a parent");
        let memory = self.assemble_memory(closed_index, model_memory, span);
        self.nodes[closed_index].memory = Some(memory);
        self.nodes[closed_index].status = NodeStatus::Closed;
        self.nodes[closed_index].end = Some(span.start);
        let parent_index = self.node_index(&parent_id);
        self.nodes[parent_index].status = NodeStatus::Live;
        self.nodes[parent_index]
            .entries
            .push(NodeEntry::Leaf(ContextItem::SourceSpan { span }));
        self.cursor = parent_id;
    }

    fn next(
        &mut self,
        span: RawSpan,
        summary: String,
        model_memory: String,
        open_input_tokens: Option<u64>,
    ) {
        let closed_id = self.cursor.clone();
        let closed_index = self.node_index(&closed_id);
        let parent_id = self.nodes[closed_index]
            .parent
            .clone()
            .expect("task node has a parent");
        let memory = self.assemble_memory(closed_index, model_memory, span);
        self.nodes[closed_index].memory = Some(memory);
        self.nodes[closed_index].status = NodeStatus::Closed;
        self.nodes[closed_index].end = Some(span.start);

        let parent_index = self.node_index(&parent_id);
        let child_ordinal = self.nodes[parent_index].children().count() as u32 + 1;
        let sibling_id = parent_id.child(child_ordinal);
        self.nodes[parent_index]
            .entries
            .push(NodeEntry::Child(sibling_id.clone()));
        self.nodes[parent_index].status = NodeStatus::Opened;
        self.nodes.push(RuntimeNode {
            id: sibling_id.clone(),
            parent: Some(parent_id),
            status: NodeStatus::Live,
            summary: Some(summary),
            memory: None,
            start: span.start,
            end: None,
            open_input_tokens,
            entries: vec![NodeEntry::Leaf(ContextItem::SourceSpan { span })],
        });
        self.cursor = sibling_id;
    }

    fn spawn(&mut self, span: RawSpan, batches: Vec<(Vec<SpawnTask>, Vec<SpawnResult>)>) {
        let parent_id = self.cursor.clone();
        let parent_index = self.node_index(&parent_id);
        let first_child_ordinal = self.nodes[parent_index].children().count() as u32 + 1;
        let total = batches.iter().map(|(tasks, _)| tasks.len()).sum();
        let mut child_ids = Vec::with_capacity(total);
        let mut children = Vec::with_capacity(total);
        for (offset, (task, result)) in batches
            .into_iter()
            .flat_map(|(tasks, results)| tasks.into_iter().zip(results))
            .enumerate()
        {
            let offset = u32::try_from(offset).unwrap_or(u32::MAX);
            let child_id = parent_id.child(first_child_ordinal.saturating_add(offset));
            let memory = vec![
                MemorySlot::SpawnEvidence {
                    owner_node: child_id.clone(),
                    source: span,
                    task: task.clone(),
                    outcome: result.outcome,
                    diagnostic: result.diagnostic,
                    execution_ref: result.execution_ref,
                },
                MemorySlot::Summary {
                    owner_node: child_id.clone(),
                    source: span,
                    body: result.memory_body,
                },
            ];
            child_ids.push(child_id.clone());
            children.push(RuntimeNode {
                id: child_id,
                parent: Some(parent_id.clone()),
                status: NodeStatus::Closed,
                summary: Some(task.summary),
                memory: Some(memory),
                start: span.start,
                end: Some(span.end),
                open_input_tokens: None,
                entries: Vec::new(),
            });
        }
        self.nodes[parent_index]
            .entries
            .push(NodeEntry::Leaf(ContextItem::SourceSpan { span }));
        self.nodes[parent_index]
            .entries
            .extend(child_ids.into_iter().map(NodeEntry::Child));
        self.nodes.extend(children);
    }

    fn apply_compact(&mut self, boundary: RawBoundary, replacement_history: Vec<ContextItem>) {
        let current_epoch = self.current_root.clone();
        for node in &mut self.nodes {
            if node.id.parts().first() == current_epoch.parts().first()
                && node.status != NodeStatus::Closed
            {
                node.status = NodeStatus::Compacted;
                node.end.get_or_insert(boundary);
            }
        }

        let next_epoch = self.current_root.parts()[0].saturating_add(1);
        let next_id = NodeId::root_epoch(next_epoch);
        self.nodes.push(RuntimeNode {
            id: next_id.clone(),
            parent: None,
            status: NodeStatus::Live,
            summary: Some("root".to_string()),
            memory: None,
            start: boundary,
            end: None,
            open_input_tokens: None,
            entries: Vec::new(),
        });
        self.baseline = replacement_history;
        self.current_root = next_id.clone();
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
        let root = &self.nodes[self.node_index(&self.current_root)];
        let mut context = self.baseline.clone();
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
                    status: NodeStatus::Opened,
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
        if self.nodes[self.node_index(&self.cursor)].parent.is_none() {
            NodeKind::RootEpoch
        } else {
            NodeKind::Task
        }
    }

    fn node_index(&self, id: &NodeId) -> usize {
        self.nodes
            .iter()
            .position(|node| &node.id == id)
            .unwrap_or_else(|| panic!("missing runtime node {id}"))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TypedTransitionError {
    MultipleStructuralFacts,
    TaskCursorRequired(&'static str),
    NonTrimFactInTrimSet,
    InactiveTrimTarget(RawBoundary),
    TrimTargetMismatch,
}

fn expire_trim_candidates(projection: &mut TrimProjection, active: &mut Vec<RawBoundary>) {
    for boundary in active.drain(..) {
        if let Some((_, TrimEdit::Tagged { eligible, .. })) = projection.edits.get_mut(&boundary) {
            *eligible = false;
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
