use super::ids::NodeId;
use std::collections::BTreeMap;
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NodeStatus {
    Live,
    Opened,
    Finished,
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
        let initial_epoch = NodeId::root_sibling(1);
        let initial_leaf = initial_epoch.child(1);
        let initial_epoch_record = NodeRecord {
            node_id: initial_epoch.clone(),
            parent_id: None,
            raw_start_ordinal: Some(initial_leaf_raw_start_ordinal),
            status: NodeStatus::Opened,
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
        visible
    }

    pub(crate) fn open(&mut self) -> Result<Transition, SpineStateError> {
        let from = self.cursor.clone();
        let child = from.child(self.next_child_index(Some(&from))?);

        self.set_status(&from, NodeStatus::Opened)?;
        self.insert_node(child.clone(), Some(from.clone()))?;
        self.cursor = child.clone();

        Ok(Transition { from, to: child })
    }

    pub(crate) fn next(
        &mut self,
        summary: impl Into<String>,
    ) -> Result<Transition, SpineStateError> {
        let from = self.cursor.clone();
        let parent = self.parent_id(&from)?;
        if parent.is_none() {
            return Err(SpineStateError::CannotAdvanceRoot);
        }
        let next_sibling = self.next_sibling_id(parent.as_ref())?;

        self.write_summary(&from, summary)?;
        self.set_status(&from, NodeStatus::Finished)?;
        self.insert_node(next_sibling.clone(), parent)?;
        self.cursor = next_sibling.clone();

        Ok(Transition {
            from,
            to: next_sibling,
        })
    }

    pub(crate) fn close(
        &mut self,
        summary: impl Into<String>,
    ) -> Result<Transition, SpineStateError> {
        let from = self.cursor.clone();
        let parent = self
            .parent_id(&from)?
            .ok_or(SpineStateError::CannotCloseRoot)?;
        let grandparent = self
            .parent_id(&parent)?
            .ok_or(SpineStateError::CannotCloseRoot)?;
        if grandparent == NodeId::root() {
            return Err(SpineStateError::CannotCloseRoot);
        }
        let parent_sibling = self.next_sibling_id(Some(&grandparent))?;

        self.write_summary(&parent, summary)?;
        self.set_status(&from, NodeStatus::Finished)?;
        self.set_status(&parent, NodeStatus::Closed)?;
        self.insert_node(parent_sibling.clone(), Some(grandparent))?;
        self.cursor = parent_sibling.clone();

        Ok(Transition {
            from,
            to: parent_sibling,
        })
    }

    pub(crate) fn reset_root_epoch(
        &mut self,
        initial_leaf_raw_start_ordinal: u64,
    ) -> Result<Transition, SpineStateError> {
        let from = NodeId::root_sibling(1);
        let to = from.child(1);
        *self = Self::new_with_initial_leaf_raw_start(initial_leaf_raw_start_ordinal);
        Ok(Transition { from, to })
    }

    pub(crate) fn root_epoch_archive_target(&self) -> Result<NodeId, SpineStateError> {
        Ok(NodeId::root_sibling(1))
    }

    pub(crate) fn root_epoch_cut_ordinal(&self) -> Result<u64, SpineStateError> {
        let root_epoch = NodeId::root_sibling(1);
        let first_leaf = self
            .nodes
            .values()
            .filter(|node| node.parent_id.as_ref() == Some(&root_epoch))
            .map(|node| node.node_id.clone())
            .min()
            .ok_or_else(|| SpineStateError::UnknownNode(root_epoch.child(1)))?;
        self.node(&first_leaf)
            .and_then(|node| node.raw_start_ordinal)
            .ok_or(SpineStateError::MissingRawStartOrdinal(first_leaf))
    }

    pub(crate) fn current_root_epoch(&self) -> Result<NodeId, SpineStateError> {
        if self.cursor == NodeId::root() {
            let first_epoch = self
                .nodes
                .values()
                .filter(|node| node.parent_id.is_none())
                .map(|node| node.node_id.clone())
                .min()
                .ok_or_else(|| SpineStateError::UnknownNode(NodeId::root_sibling(1)))?;
            return Ok(first_epoch);
        }

        let segments = self.cursor.segments();
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

    fn next_sibling_id(&self, parent_id: Option<&NodeId>) -> Result<NodeId, SpineStateError> {
        match parent_id {
            Some(parent) => Ok(parent.child(self.next_child_index(Some(parent))?)),
            None => Ok(NodeId::root_sibling(self.next_child_index(None)?)),
        }
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
    CannotAdvanceRoot,
    CannotCloseRoot,
    DuplicateNode(NodeId),
    EmptySummary,
    MissingRawStartOrdinal(NodeId),
    MissingSummary(SpineOperationName),
    SummaryAlreadyWritten(NodeId),
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
            SpineStateError::CannotAdvanceRoot => f.write_str("cannot advance the root spine node"),
            SpineStateError::CannotCloseRoot => f.write_str("cannot close the root spine node"),
            SpineStateError::MissingRawStartOrdinal(node_id) => {
                write!(f, "missing raw start ordinal for {}", node_id.bracketed())
            }
            SpineStateError::MissingSummary(op) => {
                write!(f, "spine {} requires a summary", op.as_str())
            }
            SpineStateError::SummaryAlreadyWritten(node_id) => {
                write!(f, "summary already written for {}", node_id.bracketed())
            }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpineOperationName {
    Open,
    Next,
    Close,
}

impl SpineOperationName {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            SpineOperationName::Open => "open",
            SpineOperationName::Next => "next",
            SpineOperationName::Close => "close",
        }
    }
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
