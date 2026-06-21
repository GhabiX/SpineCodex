use super::*;

// Rollback checkpoints and recovery.

#[test]
fn checkpoint_before_user_msg_records_recoverable_fields() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    runtime
        .checkpoint_before_user_msg(&rollout, 0, &[])
        .expect("write checkpoint");
    runtime.observe_raw_items(1).expect("observe raw");
    runtime
        .observe_context_item(0, 0, &text_item("first user"))
        .expect("shift user");

    let checkpoint = runtime
        .store
        .checkpoint_for_test(0)
        .expect("read checkpoint");
    assert_eq!(checkpoint.version, CHECKPOINT_VERSION);
    assert_eq!(checkpoint.checkpoint_id, "pre-user-00000000000000000000");
    assert_eq!(checkpoint.rollout_path, rollout.display().to_string());
    assert_eq!(checkpoint.raw_ordinal, 0);
    assert_eq!(checkpoint.token_seq, 2);
    assert_eq!(checkpoint.raw_live_hash, hash_raw_live(&[]));
    assert_eq!(checkpoint.context_len, 0);
    assert_eq!(checkpoint.cursor, "1.1");
    assert_eq!(
        checkpoint.parse_stack.symbols,
        vec![
            Symbol::Control(ControlSymbol::Init(
                tree_meta(
                    &runtime.archive(),
                    NodeId::root_epoch(1),
                    0,
                    "root".to_string()
                )
                .expect("root meta")
            )),
            Symbol::Control(ControlSymbol::Open(
                tree_meta(
                    &runtime.archive(),
                    NodeId::root_epoch(1).child(1),
                    0,
                    "root".to_string()
                )
                .expect("root open meta")
            )),
        ]
    );
    assert_eq!(checkpoint.tree_meta.len(), 2);
    assert!(checkpoint.memory_refs.is_empty());
    assert!(checkpoint.trajs_refs.is_empty());
    assert_eq!(
        checkpoint.h_ps_hash,
        hash_response_items(&[]).expect("hash")
    );
}

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

#[test]
fn rollback_checkpoint_without_provider_baseline_has_no_node_context() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw_after_rollback = vec![Some(text_item("kept")), None];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    runtime.observe_raw_items(1).expect("observe kept raw");
    runtime
        .observe_context_item(0, 0, &text_item("kept"))
        .expect("observe kept");
    let raw_before_rollback = vec![Some(text_item("kept"))];
    runtime
        .checkpoint_before_user_msg(&rollout, 1, &raw_before_rollback)
        .expect("write checkpoint before provider baseline");
    runtime
        .capture_current_open_provider_baseline(8_000)
        .expect("capture provider baseline after checkpoint");
    runtime
        .observe_raw_items(1)
        .expect("observe rolled-back raw");
    runtime
        .observe_context_item(1, 1, &text_item("rolled back"))
        .expect("observe rolled-back user");

    let checkpoint = runtime
        .store
        .checkpoint_for_test(1)
        .expect("read checkpoint");
    assert_eq!(checkpoint.pressure_seq_watermark, None);

    let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw_after_rollback, &[1])
        .expect("load spine")
        .expect("sidecar exists");
    assert_eq!(replayed.current_open_provider_input_tokens(), None);
}
