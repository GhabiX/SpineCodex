use super::ids::NodeId;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RawOrdinalRange {
    pub(crate) node_id: NodeId,
    pub(crate) start: u64,
    pub(crate) end: u64,
}

impl RawOrdinalRange {
    pub(crate) fn new(node_id: NodeId, start: u64, end: u64) -> Self {
        Self {
            node_id,
            start,
            end,
        }
    }
}
