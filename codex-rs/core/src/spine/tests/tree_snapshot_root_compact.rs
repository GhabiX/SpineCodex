use super::*;

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
