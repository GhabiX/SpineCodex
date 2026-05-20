use super::ids::NodeId;
use std::collections::BTreeMap;
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NodeStatus {
    Live,
    Suspended,
    Closed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NodeRecord {
    pub(crate) node_id: NodeId,
    pub(crate) parent_id: Option<NodeId>,
    pub(crate) raw_start_ordinal: Option<u64>,
    pub(crate) status: NodeStatus,
    pub(crate) summary: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpineState {
    cursor: NodeId,
    nodes: BTreeMap<NodeId, NodeRecord>,
}

impl SpineState {
    pub(crate) fn new() -> Self {
        Self::new_with_initial_leaf_raw_start(0)
    }

    pub(crate) fn new_with_initial_leaf_raw_start(initial_leaf_raw_start_ordinal: u64) -> Self {
        let initial_epoch = NodeId::root_epoch(1);
        let initial_leaf = initial_epoch.child(1);
        // The empty NodeId is only the hidden sentinel. User-visible work starts at root epoch
        // `1`, with the first live work leaf at `1.1`; do not collapse this to `1 Current`.
        let initial_epoch_record = NodeRecord {
            node_id: initial_epoch.clone(),
            parent_id: None,
            raw_start_ordinal: Some(initial_leaf_raw_start_ordinal),
            status: NodeStatus::Suspended,
            summary: None,
        };
        let initial_leaf_record = NodeRecord {
            node_id: initial_leaf.clone(),
            parent_id: Some(initial_epoch.clone()),
            raw_start_ordinal: Some(initial_leaf_raw_start_ordinal),
            status: NodeStatus::Live,
            summary: None,
        };
        Self {
            cursor: initial_leaf.clone(),
            nodes: BTreeMap::from([
                (initial_epoch, initial_epoch_record),
                (initial_leaf, initial_leaf_record),
            ]),
        }
    }

    pub(crate) fn from_records(
        cursor: NodeId,
        records: Vec<NodeRecord>,
    ) -> Result<Self, SpineStateError> {
        let mut nodes = BTreeMap::new();
        for record in records {
            let node_id = record.node_id.clone();
            if nodes.insert(node_id.clone(), record).is_some() {
                return Err(SpineStateError::DuplicateNode(node_id));
            }
        }
        if !nodes.contains_key(&cursor) {
            return Err(SpineStateError::UnknownNode(cursor));
        }
        for record in nodes.values() {
            if let Some(parent_id) = &record.parent_id
                && !nodes.contains_key(parent_id)
            {
                return Err(SpineStateError::UnknownNode(parent_id.clone()));
            }
        }
        validate_node_status_invariants(&nodes, &cursor)?;
        Ok(Self { cursor, nodes })
    }

    pub(crate) fn cursor(&self) -> &NodeId {
        &self.cursor
    }

    pub(crate) fn node(&self, node_id: &NodeId) -> Option<&NodeRecord> {
        self.nodes.get(node_id)
    }

    pub(crate) fn nodes(&self) -> &BTreeMap<NodeId, NodeRecord> {
        &self.nodes
    }

    pub(crate) fn set_raw_start_ordinal(
        &mut self,
        node_id: &NodeId,
        raw_start_ordinal: u64,
    ) -> Result<(), SpineStateError> {
        let node = self
            .nodes
            .get_mut(node_id)
            .ok_or_else(|| SpineStateError::UnknownNode(node_id.clone()))?;
        node.raw_start_ordinal = Some(raw_start_ordinal);
        Ok(())
    }

    pub(crate) fn visible_spine(&self) -> Vec<NodeId> {
        let mut visible = Vec::new();
        let mut prefix = Vec::new();
        for segment in self.cursor.segments() {
            for sibling in 1..=*segment {
                let mut sibling_path = prefix.clone();
                sibling_path.push(sibling);
                let node_id = NodeId::from_segments(sibling_path);
                if self.nodes.contains_key(&node_id) {
                    visible.push(node_id);
                }
            }
            prefix.push(*segment);
        }
        for child in self.nodes.values().filter(|node| {
            node.parent_id.as_ref() == Some(&self.cursor) && node.status == NodeStatus::Closed
        }) {
            if !visible.contains(&child.node_id) {
                visible.push(child.node_id.clone());
            }
        }
        visible
    }

    pub(crate) fn open(&mut self) -> Result<Transition, SpineStateError> {
        let from = self.cursor.clone();
        let child = from.child(self.next_child_index(Some(&from))?);

        self.set_status(&from, NodeStatus::Suspended)?;
        self.insert_node(child.clone(), Some(from.clone()))?;
        self.cursor = child.clone();

        Ok(Transition { from, to: child })
    }

    pub(crate) fn close(
        &mut self,
        summary: impl Into<String>,
    ) -> Result<Transition, SpineStateError> {
        let from = self.cursor.clone();
        let parent = self
            .parent_id(&from)?
            .ok_or(SpineStateError::CannotCloseRoot)?;

        self.write_summary(&from, summary)?;
        self.set_status(&from, NodeStatus::Closed)?;
        self.set_status(&parent, NodeStatus::Live)?;
        self.cursor = parent.clone();

        Ok(Transition { from, to: parent })
    }

    pub(crate) fn reset_root_epoch(
        &mut self,
        summary: impl Into<String>,
        initial_leaf_raw_start_ordinal: u64,
    ) -> Result<Transition, SpineStateError> {
        let from = self.current_root_epoch()?;
        let next_epoch = NodeId::root_epoch(self.next_child_index(None)?);
        let next_leaf = next_epoch.child(1);

        self.write_summary_if_absent(&from, summary)?;
        self.set_status(&from, NodeStatus::Closed)?;
        self.seal_archived_subtree(&from)?;
        self.insert_node(next_epoch.clone(), None)?;
        self.set_status(&next_epoch, NodeStatus::Suspended)?;
        self.set_raw_start_ordinal(&next_epoch, initial_leaf_raw_start_ordinal)?;
        self.insert_node(next_leaf.clone(), Some(next_epoch))?;
        self.set_raw_start_ordinal(&next_leaf, initial_leaf_raw_start_ordinal)?;
        self.cursor = next_leaf.clone();

        Ok(Transition {
            from,
            to: next_leaf,
        })
    }

    pub(crate) fn root_epoch_archive_target(&self) -> Result<NodeId, SpineStateError> {
        self.current_root_epoch()
    }

    pub(crate) fn root_epoch_cut_ordinal(&self) -> Result<u64, SpineStateError> {
        let root_epoch = self.current_root_epoch()?;
        self.node(&root_epoch)
            .and_then(|node| node.raw_start_ordinal)
            .ok_or(SpineStateError::MissingRawStartOrdinal(root_epoch))
    }

    pub(crate) fn current_root_epoch(&self) -> Result<NodeId, SpineStateError> {
        if self.cursor == NodeId::root() {
            let first_epoch = self
                .nodes
                .values()
                .filter(|node| node.parent_id.is_none())
                .map(|node| node.node_id.clone())
                .min()
                .ok_or_else(|| SpineStateError::UnknownNode(NodeId::root_epoch(1)))?;
            return Ok(first_epoch);
        }

        let segments = self.cursor.segments();
        if segments.is_empty() {
            return Err(SpineStateError::UnknownNode(self.cursor.clone()));
        }
        let root_epoch = NodeId::from_segments(vec![segments[0]]);
        if self.nodes.contains_key(&root_epoch) {
            Ok(root_epoch)
        } else {
            Err(SpineStateError::UnknownNode(root_epoch))
        }
    }

    fn parent_id(&self, node_id: &NodeId) -> Result<Option<NodeId>, SpineStateError> {
        self.nodes
            .get(node_id)
            .map(|node| node.parent_id.clone())
            .ok_or_else(|| SpineStateError::UnknownNode(node_id.clone()))
    }

    fn next_child_index(&self, parent_id: Option<&NodeId>) -> Result<u32, SpineStateError> {
        let count = self
            .nodes
            .values()
            .filter(|node| node.parent_id.as_ref() == parent_id)
            .count();
        u32::try_from(count + 1).map_err(|_| SpineStateError::TooManyChildren)
    }

    fn insert_node(
        &mut self,
        node_id: NodeId,
        parent_id: Option<NodeId>,
    ) -> Result<(), SpineStateError> {
        if self.nodes.contains_key(&node_id) {
            return Err(SpineStateError::DuplicateNode(node_id));
        }
        let node = NodeRecord {
            node_id: node_id.clone(),
            parent_id,
            raw_start_ordinal: None,
            status: NodeStatus::Live,
            summary: None,
        };
        self.nodes.insert(node_id, node);
        Ok(())
    }

    fn seal_archived_subtree(&mut self, root: &NodeId) -> Result<(), SpineStateError> {
        let descendants = self
            .nodes
            .keys()
            .filter(|node_id| *node_id != root && self.is_descendant_of(node_id, root))
            .cloned()
            .collect::<Vec<_>>();
        for node_id in descendants {
            let status = self
                .node(&node_id)
                .ok_or_else(|| SpineStateError::UnknownNode(node_id.clone()))?
                .status
                .clone();
            match status {
                NodeStatus::Live | NodeStatus::Suspended => {
                    self.set_status(&node_id, NodeStatus::Closed)?;
                }
                NodeStatus::Closed => {}
            }
        }
        Ok(())
    }

    fn is_descendant_of(&self, node_id: &NodeId, ancestor_id: &NodeId) -> bool {
        let mut parent_id = self.node(node_id).and_then(|node| node.parent_id.as_ref());
        while let Some(parent) = parent_id {
            if parent == ancestor_id {
                return true;
            }
            parent_id = self.node(parent).and_then(|node| node.parent_id.as_ref());
        }
        false
    }

    fn write_summary(
        &mut self,
        node_id: &NodeId,
        summary: impl Into<String>,
    ) -> Result<(), SpineStateError> {
        let summary = summary.into();
        if summary.trim().is_empty() {
            return Err(SpineStateError::EmptySummary);
        }
        let node = self
            .nodes
            .get_mut(node_id)
            .ok_or_else(|| SpineStateError::UnknownNode(node_id.clone()))?;
        if node.summary.is_some() {
            return Err(SpineStateError::SummaryAlreadyWritten(node_id.clone()));
        }
        node.summary = Some(summary);
        Ok(())
    }

    fn write_summary_if_absent(
        &mut self,
        node_id: &NodeId,
        summary: impl Into<String>,
    ) -> Result<(), SpineStateError> {
        if self
            .nodes
            .get(node_id)
            .ok_or_else(|| SpineStateError::UnknownNode(node_id.clone()))?
            .summary
            .is_some()
        {
            return Ok(());
        }
        self.write_summary(node_id, summary)
    }

    fn set_status(&mut self, node_id: &NodeId, status: NodeStatus) -> Result<(), SpineStateError> {
        let node = self
            .nodes
            .get_mut(node_id)
            .ok_or_else(|| SpineStateError::UnknownNode(node_id.clone()))?;
        node.status = status;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Transition {
    pub(crate) from: NodeId,
    pub(crate) to: NodeId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SpineStateError {
    ArchiveIsInternal,
    CannotCloseRoot,
    ClosedNodeHasUnfinishedDescendant {
        closed_node: NodeId,
        descendant: NodeId,
    },
    DuplicateNode(NodeId),
    EmptySummary,
    MissingRawStartOrdinal(NodeId),
    MissingSummary(SpineOperationName),
    MultipleLiveNodes {
        cursor: NodeId,
        live_nodes: Vec<NodeId>,
    },
    SummaryAlreadyWritten(NodeId),
    SuspendedNodeOutsideCursorPath {
        node_id: NodeId,
        cursor: NodeId,
    },
    TooManyChildren,
    UnexpectedSummary(SpineOperationName),
    UnknownNode(NodeId),
}

impl fmt::Display for SpineStateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SpineStateError::ArchiveIsInternal => {
                f.write_str("archive is an internal spine operation")
            }
            SpineStateError::CannotCloseRoot => f.write_str("cannot close the root spine node"),
            SpineStateError::ClosedNodeHasUnfinishedDescendant {
                closed_node,
                descendant,
            } => write!(
                f,
                "closed spine node {} has unfinished descendant {}",
                closed_node.bracketed(),
                descendant.bracketed()
            ),
            SpineStateError::MissingRawStartOrdinal(node_id) => {
                write!(f, "missing raw start ordinal for {}", node_id.bracketed())
            }
            SpineStateError::MissingSummary(op) => {
                write!(f, "spine {} requires a summary", op.as_str())
            }
            SpineStateError::MultipleLiveNodes { cursor, live_nodes } => write!(
                f,
                "spine state must have exactly one live cursor {}; found live nodes {:?}",
                cursor.bracketed(),
                live_nodes
            ),
            SpineStateError::SummaryAlreadyWritten(node_id) => {
                write!(f, "summary already written for {}", node_id.bracketed())
            }
            SpineStateError::SuspendedNodeOutsideCursorPath { node_id, cursor } => write!(
                f,
                "suspended spine node {} is outside cursor path {}",
                node_id.bracketed(),
                cursor.bracketed()
            ),
            SpineStateError::DuplicateNode(node_id) => {
                write!(f, "duplicate spine node {}", node_id.bracketed())
            }
            SpineStateError::EmptySummary => f.write_str("spine summary must not be empty"),
            SpineStateError::TooManyChildren => f.write_str("too many spine child nodes"),
            SpineStateError::UnexpectedSummary(op) => {
                write!(f, "spine {} does not accept a summary", op.as_str())
            }
            SpineStateError::UnknownNode(node_id) => {
                write!(f, "unknown spine node {}", node_id.bracketed())
            }
        }
    }
}

impl std::error::Error for SpineStateError {}

fn validate_node_status_invariants(
    nodes: &BTreeMap<NodeId, NodeRecord>,
    cursor: &NodeId,
) -> Result<(), SpineStateError> {
    let live_nodes = nodes
        .values()
        .filter(|node| node.status == NodeStatus::Live)
        .map(|node| node.node_id.clone())
        .collect::<Vec<_>>();
    if live_nodes.as_slice() != [cursor.clone()] {
        return Err(SpineStateError::MultipleLiveNodes {
            cursor: cursor.clone(),
            live_nodes,
        });
    }

    for node in nodes.values() {
        match node.status {
            NodeStatus::Live => {}
            NodeStatus::Suspended => {
                if !is_ancestor_in_records(nodes, &node.node_id, cursor) {
                    return Err(SpineStateError::SuspendedNodeOutsideCursorPath {
                        node_id: node.node_id.clone(),
                        cursor: cursor.clone(),
                    });
                }
            }
            NodeStatus::Closed => {
                for descendant in nodes.values() {
                    if descendant.node_id != node.node_id
                        && descendant.status != NodeStatus::Closed
                        && is_descendant_in_records(nodes, &descendant.node_id, &node.node_id)
                    {
                        return Err(SpineStateError::ClosedNodeHasUnfinishedDescendant {
                            closed_node: node.node_id.clone(),
                            descendant: descendant.node_id.clone(),
                        });
                    }
                }
            }
        }
    }

    Ok(())
}

fn is_ancestor_in_records(
    nodes: &BTreeMap<NodeId, NodeRecord>,
    ancestor_id: &NodeId,
    node_id: &NodeId,
) -> bool {
    ancestor_id == node_id || is_descendant_in_records(nodes, node_id, ancestor_id)
}

fn is_descendant_in_records(
    nodes: &BTreeMap<NodeId, NodeRecord>,
    node_id: &NodeId,
    ancestor_id: &NodeId,
) -> bool {
    let mut parent_id = nodes.get(node_id).and_then(|node| node.parent_id.as_ref());
    while let Some(parent) = parent_id {
        if parent == ancestor_id {
            return true;
        }
        parent_id = nodes.get(parent).and_then(|node| node.parent_id.as_ref());
    }
    false
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpineOperationName {
    Open,
    Close,
}

impl SpineOperationName {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            SpineOperationName::Open => "open",
            SpineOperationName::Close => "close",
        }
    }
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
