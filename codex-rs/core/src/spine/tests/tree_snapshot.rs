use super::*;

#[test]
fn initial_tree_snapshot_projects_root_epoch_with_live_first_child() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    let snapshot = runtime.build_tree_snapshot().expect("snapshot");
    assert_snapshot_is_self_contained_forest(&snapshot);
    let nodes = snapshot_nodes_by_id(&snapshot);

    assert_eq!(snapshot.active_node_id, "1.1");
    assert_eq!(nodes.len(), 2);
    assert_eq!(nodes["1"].parent_id, None);
    assert_eq!(nodes["1"].summary, None);
    assert_eq!(nodes["1"].status, SpineTreeNodeStatus::Opened);
    assert_eq!(nodes["1.1"].parent_id.as_deref(), Some("1"));
    assert_eq!(nodes["1.1"].summary, None);
    assert_eq!(nodes["1.1"].status, SpineTreeNodeStatus::Live);
}

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

#[test]
fn root_compact_tree_snapshot_promotes_new_root_epoch_holder() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    append_msg(&mut runtime, &mut raw, "root work");
    runtime
        .root_compact("root summary".to_string(), &raw)
        .expect("compact root");

    let snapshot = runtime.build_tree_snapshot().expect("snapshot");
    assert_snapshot_is_self_contained_forest(&snapshot);
    let nodes = snapshot_nodes_by_id(&snapshot);

    assert_eq!(snapshot.active_node_id, "2.1");
    assert_eq!(nodes["1"].parent_id, None);
    assert_eq!(nodes["1"].status, SpineTreeNodeStatus::Compacted);
    assert_eq!(nodes["2"].parent_id, None);
    assert_eq!(nodes["2"].summary, None);
    assert_eq!(nodes["2"].status, SpineTreeNodeStatus::Opened);
    assert_eq!(nodes["2.1"].parent_id.as_deref(), Some("2"));
    assert_eq!(nodes["2.1"].summary, None);
    assert_eq!(nodes["2.1"].status, SpineTreeNodeStatus::Live);
}

#[test]
fn closed_child_tree_snapshot_keeps_visible_parent_link() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    open_task(&mut runtime, &mut raw, "open-child", "child task");
    append_msg(&mut runtime, &mut raw, "child work");
    close_task(&mut runtime, &mut raw, "close-child", "1.1.1");

    let snapshot = runtime.build_tree_snapshot().expect("snapshot");
    assert_snapshot_is_self_contained_forest(&snapshot);
    let nodes = snapshot_nodes_by_id(&snapshot);

    assert_eq!(snapshot.active_node_id, "1.1");
    assert_eq!(nodes["1"].parent_id, None);
    assert_eq!(nodes["1"].status, SpineTreeNodeStatus::Opened);
    assert_eq!(nodes["1.1"].parent_id.as_deref(), Some("1"));
    assert_eq!(nodes["1.1"].status, SpineTreeNodeStatus::Live);
    assert_eq!(nodes["1.1.1"].parent_id.as_deref(), Some("1.1"));
    assert_eq!(nodes["1.1.1"].summary.as_deref(), Some("child task"));
    assert_eq!(nodes["1.1.1"].status, SpineTreeNodeStatus::Closed);
}
