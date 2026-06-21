use super::*;

#[test]
fn nested_tree_snapshot_promotes_only_missing_projection_parent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    open_task(&mut runtime, &mut raw, "open-child", "child task");

    let snapshot = runtime.build_tree_snapshot().expect("snapshot");
    assert_snapshot_is_self_contained_forest(&snapshot);
    let nodes = snapshot_nodes_by_id(&snapshot);

    assert_eq!(snapshot.active_node_id, "1.1.1");
    assert_eq!(nodes["1"].parent_id, None);
    assert_eq!(nodes["1"].status, SpineTreeNodeStatus::Opened);
    assert_eq!(nodes["1.1"].parent_id.as_deref(), Some("1"));
    assert_eq!(nodes["1.1"].status, SpineTreeNodeStatus::Opened);
    assert_eq!(nodes["1.1.1"].parent_id.as_deref(), Some("1.1"));
    assert_eq!(nodes["1.1.1"].summary.as_deref(), Some("child task"));
    assert_eq!(nodes["1.1.1"].status, SpineTreeNodeStatus::Live);
}
