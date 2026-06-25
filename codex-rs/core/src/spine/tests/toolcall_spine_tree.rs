use super::*;

#[test]
fn spine_tree_toolcall_is_plain_toolcall_leaf_for_replay_coverage() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let tree_request = spine_call(SPINE_TOOL_TREE, "tree-call");
    let tree_output = function_output("tree-call");
    let final_message = text_item("tree done");
    let raw = vec![
        Some(tree_request.clone()),
        Some(tree_output.clone()),
        Some(final_message.clone()),
    ];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime
        .observe_raw_items(1)
        .expect("record tree request raw");
    runtime
        .observe_context_item(0, 0, &tree_request)
        .expect("observe tree request");
    runtime
        .observe_raw_items(1)
        .expect("record tree output raw");
    runtime
        .observe_context_item(1, 1, &tree_output)
        .expect("observe tree output");
    runtime
        .observe_completed_toolcall(completed_toolcall(
            "tree-call",
            vec![tool_req(0, 0), tool_resp(1, 1)],
        ))
        .expect("observe completed spine.tree toolcall");
    runtime
        .observe_raw_items(1)
        .expect("record final message raw");
    runtime
        .observe_context_item(2, 2, &final_message)
        .expect("observe final message");

    assert_eq!(
        runtime
            .materialize_history_for_test(&raw)
            .expect("tree request/output stay ordinary toolcall"),
        vec![
            tree_request.clone(),
            tree_output.clone(),
            anchored_text_item(1, "tree done")
        ]
    );
    let replayed = SpineRuntime::load_for_rollout(&rollout, 3)
        .expect("tree toolcall should replay without missing coverage")
        .expect("sidecar exists");
    assert_eq!(
        replayed
            .materialize_history_for_test(&raw)
            .expect("replayed tree request/output stay ordinary toolcall"),
        vec![
            tree_request,
            tree_output,
            anchored_text_item(1, "tree done")
        ]
    );
}
