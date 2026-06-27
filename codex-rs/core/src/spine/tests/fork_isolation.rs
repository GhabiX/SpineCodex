use super::*;

// Fork isolation and replayed materialization.

#[test]
fn fork_clone_rewrites_node_dirs_copies_artifacts_and_isolates_parent() {
    assert_fork_clone_rewrites_node_dirs_copies_artifacts_and_isolates_parent();
}

pub(super) fn assert_fork_clone_rewrites_node_dirs_copies_artifacts_and_isolates_parent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let parent_rollout = dir.path().join("parent.jsonl");
    let child_rollout = dir.path().join("child.jsonl");
    let mut raw = Vec::new();
    let mut parent = SpineRuntime::load_or_create(&parent_rollout, 0).expect("create parent");

    append_msg(&mut parent, &mut raw, "parent root before child");
    open_task(&mut parent, &mut raw, "open-child", "child task");
    append_msg(&mut parent, &mut raw, "child work");
    close_task(&mut parent, &mut raw, "close-child", "1.1.1");
    append_msg(&mut parent, &mut raw, "parent after child");

    let parent_materialized = parent
        .materialize_history_for_test(&raw)
        .expect("parent h(PS)");
    let parent_stack_before_child_work = parent.parse_stack().clone();
    let parent_tree_events_before_child_work = event_log_debug(&parent);
    let parent_root = parent.store.root.clone();

    let raw_live = vec![true; raw.len()];
    clone_for_rollout_with_raw_live(&parent_rollout, &child_rollout, &raw_live);
    let child = SpineRuntime::load_for_rollout_items(&child_rollout, &raw, &[])
        .expect("load child")
        .expect("child sidecar exists");
    let child_root = child.store.root.clone();

    assert_ne!(child_root, parent_root);
    assert_eq!(
        child
            .materialize_history_for_test(&raw)
            .expect("child h(PS)"),
        parent_materialized,
        "fork child h(PS) must match parent at fork boundary"
    );

    let Some(Symbol::SpineTreeNodes(nodes)) = child.parse_stack().symbols.last() else {
        panic!("fork child should replay parent root nodes");
    };
    let child_meta_dir = match nodes.as_slice() {
        [
            SpineTreeNode::MsgAsLeafNode { .. },
            SpineTreeNode::SpineTree {
                meta,
                memory_path,
                trajs_path,
                children,
                ..
            },
            SpineTreeNode::ToolCallAsLeafNode { segments },
            SpineTreeNode::MsgAsLeafNode { .. },
        ] if segments == &vec![tool_req(4, 2), tool_resp(5, 3)] => {
            assert_eq!(meta.id, NodeId::root_epoch(1).child(1).child(1));
            assert!(meta.node_dir.starts_with(&child_root));
            assert!(!meta.node_dir.starts_with(&parent_root));
            assert_eq!(memory_path, &PathBuf::from("nodes/1/1/1/Memory.md"));
            assert_eq!(trajs_path, &PathBuf::from("nodes/1/1/1/Trajs.md"));
            assert!(matches!(
                children.as_slice(),
                [
                    SpineTreeNode::ToolCallAsLeafNode { segments },
                    SpineTreeNode::MsgAsLeafNode { .. },
                ] if segments == &vec![tool_req(1, 1), tool_resp(2, 2)]
            ));
            meta.node_dir.clone()
        }
        other => panic!("unexpected fork child nodes: {other:?}"),
    };
    let child_memory_archive =
        std::fs::read_to_string(child_meta_dir.join("Memory.md")).expect("child Memory.md");
    let child_trajs_archive =
        std::fs::read_to_string(child_meta_dir.join("Trajs.md")).expect("child Trajs.md");
    assert!(child_memory_archive.contains("Spine Memory 1.1.1"));
    assert!(child_trajs_archive.contains("raw raw_ordinal=3"));
    assert!(child_trajs_archive.contains("context_index=1"));
    assert!(child_meta_dir.join("Memory.md").exists());
    assert!(child_meta_dir.join("Trajs.md").exists());

    let mut child = child;
    open_task(&mut child, &mut raw, "child-open-only", "child-only task");
    append_msg(&mut child, &mut raw, "child-only work");
    close_task(&mut child, &mut raw, "child-close-only", "1.1.2");

    let reloaded_parent = SpineRuntime::load_for_rollout(&parent_rollout, parent.raw_len)
        .expect("reload parent")
        .expect("parent sidecar exists");
    assert_eq!(
        reloaded_parent.parse_stack(),
        &parent_stack_before_child_work
    );
    assert_eq!(
        event_log_debug(&reloaded_parent),
        parent_tree_events_before_child_work
    );
}
