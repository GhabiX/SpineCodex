use super::*;

#[test]
fn initial_checkpoint_records_root_open_without_msg() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime
        .checkpoint_initial(&rollout, &[])
        .expect("write initial checkpoint");
    let checkpoint = runtime
        .store
        .initial_checkpoint_for_test()
        .expect("read initial checkpoint");

    assert_eq!(checkpoint.checkpoint_id, "initial");
    assert_eq!(checkpoint.raw_ordinal, 0);
    assert_eq!(checkpoint.context_len, 0);
    assert_eq!(checkpoint.cursor, "1.1");
    assert!(checkpoint.memory_refs.is_empty());
    assert!(checkpoint.trajs_refs.is_empty());
    assert!(matches!(
        checkpoint.parse_stack.symbols.as_slice(),
        [
            Symbol::Control(ControlSymbol::Init(_)),
            Symbol::Control(ControlSymbol::Open(root))
        ] if root.id == NodeId::root_epoch(1).child(1)
            && root.summary == "root"
    ));
}
