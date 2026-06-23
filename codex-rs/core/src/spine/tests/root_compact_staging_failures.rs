use super::*;

#[test]
fn root_compact_staging_failure_does_not_write_memory_body() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let init_meta = crate::spine::archive::tree_meta(
        &runtime.archive(),
        NodeId::root_epoch(1),
        0,
        "root".to_string(),
    )
    .expect("build init meta");
    let open_meta = crate::spine::archive::tree_meta(
        &runtime.archive(),
        NodeId::root_epoch(1).child(1),
        0,
        "root".to_string(),
    )
    .expect("build open meta");
    let close_memory = crate::spine::archive::memory_ref(
        &runtime.archive(),
        "invalid-close".to_string(),
        NodeId::root_epoch(1).child(1),
        crate::spine::io::sha1_hex("invalid close".as_bytes()),
        0..0,
        0..0,
        1..2,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    );
    runtime.parse_stack_mut_for_test().symbols = vec![
        crate::spine::model::Symbol::Control(crate::spine::model::ControlSymbol::Init(init_meta)),
        crate::spine::model::Symbol::Control(crate::spine::model::ControlSymbol::Open(open_meta)),
        crate::spine::model::Symbol::Control(crate::spine::model::ControlSymbol::Close(
            close_memory,
        )),
    ];
    let parse_stack_before = runtime.parse_stack().clone();
    let events_before = event_log_debug(&runtime);
    let mem_count_before = runtime
        .store
        .mems()
        .expect("read mems before failure")
        .len();
    let compact_checkpoint_count_before = runtime
        .store
        .compact_checkpoints()
        .expect("read checkpoints before failure")
        .len();

    let err = runtime
        .root_compact_with_checkpoint(
            &rollout,
            "root summary after invalid close".to_string(),
            &[],
            SpineRootCompactTokenMetadata::default(),
        )
        .expect_err("invalid staged parse stack should fail before commit");
    assert!(
        err.to_string()
            .contains("spine.close requires non-empty live suffix"),
        "unexpected staging failure error: {err}"
    );

    assert_eq!(runtime.parse_stack(), &parse_stack_before);
    assert_eq!(event_log_debug(&runtime), events_before);
    assert_eq!(
        runtime.store.mems().expect("read mems after failure").len(),
        mem_count_before
    );
    assert_eq!(
        runtime
            .store
            .compact_checkpoints()
            .expect("read checkpoints after failure")
            .len(),
        compact_checkpoint_count_before
    );
    assert!(
        !runtime.store.root.join("memory/root-1-0.md").exists(),
        "root compact body must not be written before staging succeeds"
    );
}
