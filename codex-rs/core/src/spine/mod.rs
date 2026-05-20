pub(crate) mod candidate_mem_plan;
pub(crate) mod checkpoint_render;
pub(crate) mod compact;
pub(crate) mod fast_fail;
pub(crate) mod host_bridge;
pub(crate) mod ids;
pub(crate) mod instructions;
pub(crate) mod mem_install;
pub(crate) mod project_pi;
pub(crate) mod projection;
pub(crate) mod projection_epoch;
pub(crate) mod runtime;
pub(crate) mod segment;
pub(crate) mod session_integration;
pub(crate) mod size_hint;
pub(crate) mod state;
pub(crate) mod store;
pub(crate) mod trajs;
pub(crate) mod tree_snapshot;
pub(crate) mod view;

pub(crate) const SPINE_NAMESPACE: &str = "spine";
pub(crate) const SPINE_TOOL_OPEN: &str = "open";
pub(crate) const SPINE_TOOL_CLOSE: &str = "close";
pub(crate) const SPINE_TOOL_TREE: &str = "tree";

pub(crate) fn is_spine_transition_tool(name: &str, namespace: Option<&str>) -> bool {
    namespace == Some(SPINE_NAMESPACE) && matches!(name, SPINE_TOOL_OPEN | SPINE_TOOL_CLOSE)
}

pub(crate) fn is_spine_shaped_history_tool(name: &str, namespace: Option<&str>) -> bool {
    let _ = name;
    namespace == Some(SPINE_NAMESPACE)
}
