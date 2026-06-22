use super::*;

#[test]
fn rollback_checkpoint_replays_new_live_append_after_cut() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw_after_rollback = vec![
        Some(text_item("kept")),
        None,
        Some(text_item("after rollback")),
    ];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    runtime.observe_raw_items(1).expect("observe kept raw");
    runtime
        .observe_context_item(0, 0, &text_item("kept"))
        .expect("observe kept");
    let raw_before_rollback = vec![Some(text_item("kept"))];
    runtime
        .checkpoint_before_user_msg(&rollout, 1, &raw_before_rollback)
        .expect("write checkpoint");
    runtime
        .observe_raw_items(1)
        .expect("observe rolled-back raw");
    runtime
        .observe_context_item(1, 1, &text_item("rolled back"))
        .expect("observe rolled-back user");
    runtime.observe_raw_items(1).expect("observe new raw");
    runtime
        .observe_context_item(2, 1, &text_item("after rollback"))
        .expect("observe new user");

    let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw_after_rollback, &[1])
        .expect("load spine")
        .expect("sidecar exists");

    assert_eq!(
        replayed
            .materialize_history(&raw_after_rollback)
            .expect("materialize"),
        vec![
            anchored_text_item(1, "kept"),
            anchored_text_item(3, "after rollback")
        ]
    );
    let Some(Symbol::SpineTreeNodes(nodes)) = replayed.parse_stack().symbols.last() else {
        panic!("expected root nodes after replay")
    };
    assert!(matches!(
        nodes.as_slice(),
        [
            SpineTreeNode::MsgAsLeafNode {
                msg: SegRef::ResponseItem {
                    raw_ordinal: 0,
                    context_index: 0,
                },
                ..
            },
            SpineTreeNode::MsgAsLeafNode {
                msg: SegRef::ResponseItem {
                    raw_ordinal: 2,
                    context_index: 1,
                },
                ..
            },
        ]
    ));
}
