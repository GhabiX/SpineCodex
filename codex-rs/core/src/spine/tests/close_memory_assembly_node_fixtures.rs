use crate::spine::NodeId;

pub(super) fn node_id(path: &[u32]) -> NodeId {
    serde_json::from_value(serde_json::json!(path)).expect("node id")
}
