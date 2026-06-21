use super::*;

#[test]
fn tree_snapshot_hides_closed_historical_subtree_descendants() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    open_task(
        &mut runtime,
        &mut raw,
        "open-historical",
        "historical parent",
    );
    open_task(&mut runtime, &mut raw, "open-child", "historical child");
    append_msg(&mut runtime, &mut raw, "historical child work");
    close_task(&mut runtime, &mut raw, "close-child", "1.1.1.1");
    append_msg(&mut runtime, &mut raw, "historical parent work");
    next_task(
        &mut runtime,
        &mut raw,
        "next-sibling",
        "1.1.1",
        "current sibling",
    );
    open_task(&mut runtime, &mut raw, "open-active-child", "active child");

    let tree = runtime.render_tree().expect("render tree");
    assert!(tree.contains("[1.1.1] Done historical parent"), "{tree}");
    assert!(!tree.contains("[1.1.1.1] Done historical child"), "{tree}");

    let snapshot = runtime.build_tree_snapshot().expect("snapshot");
    assert_snapshot_is_self_contained_forest(&snapshot);
    let nodes = snapshot_nodes_by_id(&snapshot);

    assert_eq!(snapshot.active_node_id, "1.1.2.1");
    assert!(nodes.contains_key("1"));
    assert!(nodes.contains_key("1.1"));
    assert_eq!(nodes["1.1.1"].parent_id.as_deref(), Some("1.1"));
    assert_eq!(nodes["1.1.1"].status, SpineTreeNodeStatus::Closed);
    assert_eq!(nodes["1.1.1"].summary.as_deref(), Some("historical parent"));
    assert!(nodes.contains_key("1.1.2"));
    assert!(nodes.contains_key("1.1.2.1"));
    assert!(
        !nodes.contains_key("1.1.1.1"),
        "closed historical descendants must stay out of the TUI snapshot: {snapshot:?}"
    );
}
