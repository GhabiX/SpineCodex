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
use crate::RawBoundary;
use crate::RawSpan;
use crate::RolloutEvent;
use crate::SpawnArrival;
use crate::SpawnState;
use crate::SpawnTask;
use crate::SpineError;
use crate::SpineOperationFact;
use crate::SpineProjection;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq)]
enum NodeEntry {
    Leaf(ContextItem),
    Child(NodeId),
    /// The agent's reply inside a sampling span. The span renders it while the
    /// node is open; the node's memory keeps it after Close.
    Reply(Message),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RuntimeNode {
    id: NodeId,
    parent: Option<NodeId>,
    kind: NodeKind,
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
            NodeEntry::Leaf(_) | NodeEntry::Reply(_) => None,
        })
    }

    fn snapshot(
        &self,
        arrivals: &BTreeMap<NodeId, SpawnArrival>,
        inherited: &BTreeSet<NodeId>,
    ) -> NodeSnapshot {
        NodeSnapshot {
            id: self.id.clone(),
            parent: self.parent.clone(),
            children: self.children().cloned().collect(),
            kind: self.kind,
            status: self.status,
            summary: self.summary.clone(),
            memory: self.memory.clone(),
            spawn: (self.kind == NodeKind::Spawn).then(|| match arrivals.get(&self.id) {
                Some(arrival) => SpawnState::Arrived(arrival.outcome),
                None if inherited.contains(&self.id) => SpawnState::Inherited,
                None => SpawnState::Pending,
            }),
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
    /// Every admitted arrival, by producer.
    arrivals: BTreeMap<NodeId, SpawnArrival>,
    /// Births unarrived when this namespace continued an earlier one.
    inherited: BTreeSet<NodeId>,
    last_boundary: Option<RawBoundary>,
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
                kind: NodeKind::RootEpoch,
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
            arrivals: BTreeMap::new(),
            inherited: BTreeSet::new(),
            last_boundary: None,
        }
    }

    pub(crate) fn apply(&mut self, event: RolloutEvent) -> Result<SpineProjection, SpineError> {
        self.last_boundary = Some(event.boundary());
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
            RolloutEvent::Arrival { boundary, arrival } => self.admit_arrival(boundary, arrival)?,
            RolloutEvent::Compact {
                boundary,
                replacement_history,
            } => {
                let unarrived = self.unarrived_spawns(&self.current_root);
                if !unarrived.is_empty() {
                    return Err(SpineError::UnarrivedSpawns(unarrived));
                }
                self.apply_compact(boundary, replacement_history);
            }
        }
        Ok(self.projection())
    }

    pub(crate) fn apply_sampling(
        &mut self,
        span: RawSpan,
        facts: &[&ExecutedSpineFact],
        replies: Vec<Message>,
        open_input_tokens: Option<u64>,
    ) -> Result<SpineProjection, TypedTransitionError> {
        self.last_boundary = Some(span.end);
        let structural = facts.to_vec();
        if !structural.is_empty() && !replies.is_empty() {
            return Err(TypedTransitionError::ReplyWithTransition);
        }
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
                    SpineOperationFact::Spawn { tasks } => Some(tasks.clone()),
                    SpineOperationFact::Open { .. }
                    | SpineOperationFact::Close { .. }
                    | SpineOperationFact::Next { .. } => None,
                })
                .collect();
            self.spawn(span, batches);
        } else {
            match structural.first().map(|fact| &fact.operation) {
                Some(SpineOperationFact::Open { summary }) => {
                    self.open(span, summary.clone(), open_input_tokens);
                }
                Some(SpineOperationFact::Close { memory }) => {
                    self.require_closable("close")?;
                    self.close(span, memory.clone());
                }
                Some(SpineOperationFact::Next {
                    closed_memory,
                    next_summary,
                }) => {
                    self.require_closable("next")?;
                    self.next(
                        span,
                        next_summary.clone(),
                        closed_memory.clone(),
                        open_input_tokens,
                    );
                }
                Some(SpineOperationFact::Spawn { tasks }) => {
                    self.spawn(span, vec![tasks.clone()]);
                }
                None => {
                    self.push_source_span(span);
                    // While a birth is unarrived the turn goes on, so a reply
                    // then only reports waiting and is not kept.
                    if self.unarrived_spawns(&self.current_root).is_empty() {
                        for reply in replies {
                            self.push_cursor_entry(NodeEntry::Reply(reply));
                        }
                    }
                }
            }
        }

        Ok(self.projection())
    }

    pub(crate) fn projection(&self) -> SpineProjection {
        SpineProjection {
            nodes: self
                .nodes
                .iter()
                .map(|node| node.snapshot(&self.arrivals, &self.inherited))
                .collect(),
            cursor: self.cursor.clone(),
            visible_context: self.render_current_epoch(),
            last_boundary: self.last_boundary,
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

    /// Admits a spawned child's final result at the cursor. The producer must
    /// be a birth of the current epoch that has not arrived yet. The same
    /// arrival admitted again is a no-op, so a host may deliver an arrival
    /// again until it sees it admitted; a different result is an error.
    fn admit_arrival(
        &mut self,
        boundary: RawBoundary,
        arrival: SpawnArrival,
    ) -> Result<(), SpineError> {
        arrival.validate().map_err(SpineError::InvalidArrival)?;
        if let Some(admitted) = self.arrivals.get(&arrival.producer) {
            return if *admitted == arrival {
                Ok(())
            } else {
                Err(SpineError::DuplicateArrival(arrival.producer))
            };
        }
        let born = self.nodes.iter().any(|node| {
            node.kind == NodeKind::Spawn
                && node.id == arrival.producer
                && node.id.is_within(&self.current_root)
        });
        if !born {
            return Err(SpineError::UnknownSpawnProducer(arrival.producer));
        }
        self.arrivals
            .insert(arrival.producer.clone(), arrival.clone());
        let owner_node = self.cursor.clone();
        self.push_cursor_entry(NodeEntry::Leaf(ContextItem::MemorySlot(
            MemorySlot::Arrival {
                owner_node,
                boundary,
                arrival,
            },
        )));
        Ok(())
    }

    /// Births inside `scope` this namespace still has to join.
    fn unarrived_spawns(&self, scope: &NodeId) -> Vec<NodeId> {
        self.nodes
            .iter()
            .filter(|node| {
                node.kind == NodeKind::Spawn
                    && node.id.is_within(scope)
                    && !self.arrivals.contains_key(&node.id)
                    && !self.inherited.contains(&node.id)
            })
            .map(|node| node.id.clone())
            .collect()
    }

    /// Continuing in a new namespace: a birth belongs to the namespace that
    /// bore it, so the ones still unarrived become inherited context here.
    pub(crate) fn inherit_pending_spawns(&mut self) {
        let pending = self.unarrived_spawns(&self.current_root);
        self.inherited.extend(pending);
    }

    fn require_closable(&self, operation: &'static str) -> Result<(), TypedTransitionError> {
        if self.nodes[self.node_index(&self.cursor)].kind != NodeKind::Task {
            return Err(TypedTransitionError::TaskCursorRequired(operation));
        }
        let unarrived = self.unarrived_spawns(&self.cursor);
        if !unarrived.is_empty() {
            return Err(TypedTransitionError::UnarrivedSpawns(unarrived));
        }
        Ok(())
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
            kind: NodeKind::Task,
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
            kind: NodeKind::Task,
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

    fn spawn(&mut self, span: RawSpan, batches: Vec<Vec<SpawnTask>>) {
        let parent_id = self.cursor.clone();
        let parent_index = self.node_index(&parent_id);
        let first_child_ordinal = self.nodes[parent_index].children().count() as u32 + 1;
        let mut child_ids = Vec::new();
        let mut children = Vec::new();
        for (offset, task) in batches.into_iter().flatten().enumerate() {
            let offset = u32::try_from(offset).unwrap_or(u32::MAX);
            let child_id = parent_id.child(first_child_ordinal.saturating_add(offset));
            child_ids.push(child_id.clone());
            children.push(RuntimeNode {
                id: child_id.clone(),
                parent: Some(parent_id.clone()),
                kind: NodeKind::Spawn,
                status: NodeStatus::Closed,
                summary: Some(task.summary.clone()),
                memory: Some(vec![MemorySlot::Birth {
                    owner_node: child_id,
                    source: span,
                    task,
                }]),
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
            kind: NodeKind::RootEpoch,
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
        // The summary leads, so what the node keeps (its users' messages,
        // replies, arrivals and children's memories, in order) follows the
        // account of the work behind it.
        let mut slots = vec![MemorySlot::Summary {
            owner_node: owner_node.clone(),
            source,
            body: model_memory,
        }];
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
                NodeEntry::Leaf(ContextItem::MemorySlot(slot @ MemorySlot::Arrival { .. })) => {
                    slots.push(slot.clone());
                }
                NodeEntry::Reply(message) => slots.push(MemorySlot::Reply {
                    owner_node: owner_node.clone(),
                    message: message.clone(),
                }),
                NodeEntry::Child(child_id) => {
                    let child = &self.nodes[self.node_index(child_id)];
                    if let Some(memory) = &child.memory {
                        slots.extend(memory.iter().cloned());
                    }
                }
                NodeEntry::Leaf(_) => {}
            }
        }
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
                NodeEntry::Reply(_) => {}
            }
        }
    }

    fn push_cursor_entry(&mut self, entry: NodeEntry) {
        let index = self.node_index(&self.cursor);
        self.nodes[index].entries.push(entry);
    }

    fn node_index(&self, id: &NodeId) -> usize {
        self.nodes
            .iter()
            .position(|node| &node.id == id)
            .unwrap_or_else(|| panic!("missing runtime node {id}"))
    }
}

pub(crate) fn display_nodes(nodes: &[NodeId]) -> String {
    nodes
        .iter()
        .map(NodeId::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TypedTransitionError {
    MultipleStructuralFacts,
    TaskCursorRequired(&'static str),
    UnarrivedSpawns(Vec<NodeId>),
    ReplyWithTransition,
}
