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
        let root = NodeId::root();
        let root_record = NodeRecord {
            node_id: root.clone(),
            parent_id: None,
            raw_start_ordinal: Some(0),
            status: NodeStatus::Live,
            summary: None,
        };
        Self {
            cursor: root.clone(),
            nodes: BTreeMap::from([(root, root_record)]),
        }
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

    pub(crate) fn open(
        &mut self,
        summary: impl Into<String>,
    ) -> Result<Transition, SpineStateError> {
        let from = self.cursor.clone();
        let child = from.child(self.next_child_index(Some(&from))?);

        self.write_summary(&from, summary)?;
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
        let parent_sibling = self.next_sibling_id(Some(&grandparent))?;

        self.write_summary(&from, summary)?;
        self.set_status(&from, NodeStatus::Finished)?;
        self.set_status(&parent, NodeStatus::Closed)?;
        self.insert_node(parent_sibling.clone(), Some(grandparent))?;
        self.cursor = parent_sibling.clone();

        Ok(Transition {
            from,
            to: parent_sibling,
        })
    }

    pub(crate) fn archive_current_root_epoch(
        &mut self,
        summary: impl Into<String>,
    ) -> Result<Transition, SpineStateError> {
        let archived = self.current_root_epoch()?;
        let parent = self.parent_id(&archived)?;
        let next_epoch = self.next_sibling_id(parent.as_ref())?;

        self.write_summary_if_absent(&archived, summary)?;
        self.set_status(&archived, NodeStatus::Closed)?;
        self.insert_node(next_epoch.clone(), parent)?;
        self.cursor = next_epoch.clone();

        Ok(Transition {
            from: archived,
            to: next_epoch,
        })
    }

    pub(crate) fn current_root_epoch(&self) -> Result<NodeId, SpineStateError> {
        if self.cursor == NodeId::root() {
            return Ok(self.cursor.clone());
        }

        let segments = self.cursor.segments();
        let root_epoch = if segments.first() == Some(&1) && segments.len() > 1 {
            NodeId::from_segments(vec![1, segments[1]])
        } else {
            NodeId::from_segments(vec![segments[0]])
        };
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
    CannotCloseRoot,
    DuplicateNode(NodeId),
    EmptySummary,
    SummaryAlreadyWritten(NodeId),
    TooManyChildren,
    UnknownNode(NodeId),
}

impl fmt::Display for SpineStateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SpineStateError::CannotCloseRoot => f.write_str("cannot close the root spine node"),
            SpineStateError::SummaryAlreadyWritten(node_id) => {
                write!(f, "summary already written for {}", node_id.bracketed())
            }
            SpineStateError::DuplicateNode(node_id) => {
                write!(f, "duplicate spine node {}", node_id.bracketed())
            }
            SpineStateError::EmptySummary => f.write_str("spine summary must not be empty"),
            SpineStateError::TooManyChildren => f.write_str("too many spine child nodes"),
            SpineStateError::UnknownNode(node_id) => {
                write!(f, "unknown spine node {}", node_id.bracketed())
            }
        }
    }
}

impl std::error::Error for SpineStateError {}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
