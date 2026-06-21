use super::*;

#[test]
fn empty_task_tree_reduce_fails_without_archive_side_effects() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let archive = runtime.archive();
    let node_id = NodeId::root_epoch(1).child(1);
    let open = Symbol::Control(ControlSymbol::Open(
        tree_meta(&archive, node_id.clone(), 0, "empty".to_string()).expect("meta"),
    ));
    let memory = memory_ref(
        &archive,
        "empty-memory".to_string(),
        node_id,
        sha1_hex(b"empty"),
        0..0,
        0..0,
        0..0,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    );
    let mut parse_stack = ParseStack {
        symbols: vec![open, Symbol::Control(ControlSymbol::Close(memory))],
    };

    let err = parse_stack
        .shift(SpineToken::End, &archive)
        .expect_err("open close without Nodes must fail");
    assert!(
        err.to_string()
            .contains("spine.close requires non-empty live suffix"),
        "unexpected empty task close error: {err}"
    );
    assert!(
        !runtime.store.root.join("nodes/1/1").exists(),
        "empty close must not archive a TaskTree"
    );
}

#[test]
fn open_toolcall_leaf_makes_close_suffix_non_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(0, 0, &spine_call(SPINE_TOOL_OPEN, "open"))
        .expect("observe open request");
    runtime
        .stage_open("open".to_string(), "empty child".to_string())
        .expect("stage open");
    runtime.observe_raw_items(1).expect("record open output");
    runtime
        .observe_context_item(1, 1, &function_output("open"))
        .expect("observe open output");
    runtime
        .maybe_commit_output("open", None)
        .expect("commit open");
    runtime.observe_raw_items(1).expect("record close request");
    runtime
        .observe_context_item(2, 2, &spine_call(SPINE_TOOL_CLOSE, "close"))
        .expect("observe close request");
    runtime
        .stage_close("close".to_string(), "test node memory".to_string())
        .expect("stage close");
    runtime.observe_raw_items(1).expect("record close output");
    runtime
        .observe_context_item(3, 3, &function_output("close"))
        .expect("observe close output");

    let commit = runtime
        .maybe_commit_output(
            "close",
            Some(memory_assembly_with_context_range("1.1.1", 0..2)),
        )
        .expect("close open-only child")
        .expect("close should commit");
    assert!(matches!(commit, SpineCommitKind::Close));
    assert_eq!(runtime.store.mems().expect("read mems").len(), 1);
    assert!(
        runtime.store.root.join("memory/mem-1-1-1-0-2.md").exists(),
        "close must archive memory for the open toolcall suffix"
    );
    assert!(
        runtime.store.root.join("nodes/1/1/1").exists(),
        "close must archive the child TaskTree"
    );
}
