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
