//! Host-neutral artifact facts derived from a Spine projection.

use crate::MemorySlot;
use crate::NodeId;
use crate::NodeKind;
use crate::NodeStatus;
use crate::SpineProjection;

pub const TRIM_SNIPPED_BODY: &str = "[Old tool result content cleared]";

pub fn render_memory_artifact(node_id: &NodeId, body: &str) -> String {
    format!("# Spine Memory {node_id}\n\n## Node Memory\n{body}")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryArtifact {
    pub node_id: NodeId,
    pub summary: String,
    pub body: String,
}

pub fn closed_memory_artifacts(projection: &SpineProjection) -> Vec<MemoryArtifact> {
    projection
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Task && node.status == NodeStatus::Closed)
        .filter_map(|node| {
            let body = node.memory.as_ref()?.iter().find_map(|slot| match slot {
                MemorySlot::Summary {
                    owner_node, body, ..
                } if owner_node == &node.id => Some(body.clone()),
                _ => None,
            })?;
            Some(MemoryArtifact {
                node_id: node.id.clone(),
                summary: node.summary.clone().unwrap_or_else(|| "node".to_string()),
                body,
            })
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserMessageArtifact {
    pub anchor: u64,
    pub body: String,
}
