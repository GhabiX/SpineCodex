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
