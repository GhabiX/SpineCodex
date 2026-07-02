use super::*;
use crate::spine::io::hash_raw_live;
use crate::spine::model::RawMask;

#[test]
fn rollback_hole_allows_suffix_memory_with_matching_raw_live_proof() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw = vec![
        Some(text_item("before")),
        Some(spine_call(SPINE_TOOL_OPEN, "open")),
        Some(function_output("open")),
        None,
        Some(spine_call(SPINE_TOOL_CLOSE, "close")),
        Some(function_output("close")),
    ];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 1).expect("create spine");
    runtime
        .observe_context_item(0, 0, &text_item("before"))
        .expect("observe prefix");
    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(1, 1, &spine_call(SPINE_TOOL_OPEN, "open"))
        .expect("observe open request");
    runtime
        .stage_open("open".to_string(), "child task".to_string())
        .expect("stage open");
    runtime.observe_raw_items(1).expect("record open output");
    runtime
        .observe_context_item(2, 2, &function_output("open"))
        .expect("observe open output");
    runtime
        .maybe_commit_output("open", None)
        .expect("commit open");
    runtime
        .checkpoint_before_user_msg(&rollout, 3, &raw[..3])
        .expect("write rollback checkpoint");
    runtime
        .observe_raw_items(1)
        .expect("record rolled-back child raw");
    runtime
        .observe_context_item(3, 3, &text_item("rolled back child"))
        .expect("observe rolled-back child raw");
    runtime.observe_raw_items(1).expect("record close request");
    runtime
        .observe_context_item(4, 3, &spine_call(SPINE_TOOL_CLOSE, "close"))
        .expect("observe close request after live rollback");
    runtime
        .stage_close("close".to_string(), "test node memory".to_string())
        .expect("stage close");
    let suffix_start = match runtime.pending_commit("close").expect("pending close") {
        Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
        other => panic!("expected pending close, got {other:?}"),
    };
    let suffix_raw_start = u64::try_from(suffix_start).expect("suffix start fits u64");
    runtime.observe_raw_items(1).expect("record close output");
    runtime
        .observe_context_item(5, 4, &function_output("close"))
        .expect("observe close output");
    runtime
        .maybe_commit_output(
            "close",
            Some(memory_assembly_with_ranges(
                "1.1.1",
                1..5,
                suffix_raw_start..5,
            )),
        )
        .expect("commit close");

    let mems = runtime.store.mems().expect("read committed memory");
    let mem = mems
        .iter()
        .find(|mem| mem.kind == MemKind::Suffix && mem.raw_end == 5)
        .expect("committed close memory");
    let expected_raw_live_hash = hash_raw_live(&[true, true, true, false, true]);
    assert_eq!(
        mem.raw_live_hash.as_deref(),
        Some(expected_raw_live_hash.as_str())
    );

    let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw, &[])
        .expect("load sparse rollback memory")
        .expect("sidecar exists");
    let materialized = replayed
        .materialize_variable_context_for_test(&raw)
        .expect("materialize sparse rollback memory");
    assert_eq!(materialized.get(0), Some(&anchored_text_item(1, "before")));
    assert_eq!(
        materialized.get(1),
        Some(&memory_response_item(
            "# Spine Memory 1.1.1\n\nreal compact body for 1.1.1\n"
        ))
    );
}

#[test]
fn legacy_suffix_memory_without_raw_live_hash_rejects_rollback_hole() {
    let raw_live = [true, true, true, false, true];
    let legacy = MemRecord {
        compact_id: "legacy-mem".to_string(),
        kind: MemKind::Suffix,
        node: NodeId::root_epoch(1).child(1),
        raw_start: 1,
        raw_end: 5,
        context_start: 1,
        context_end: 2,
        rendered_context_item_count: None,
        raw_live_hash: None,
        open_input_tokens: None,
        close_input_tokens: None,
        open_context_tokens: None,
        close_context_tokens: None,
        closed_source_suffix_tokens: None,
        closed_memory_context_tokens: None,
        open_context_source: None,
        memory_output_tokens: None,
        body_path: "memory/legacy-mem.md".to_string(),
        body_hash: sha1_hex(b"legacy"),
    };

    assert!(
        !legacy
            .allowed_by(RawMask::new(&raw_live))
            .expect("legacy coverage check"),
        "legacy suffix memory without proof must not cover a rollback hole"
    );

    let proved = MemRecord {
        raw_live_hash: Some(hash_raw_live(&raw_live)),
        ..legacy
    };
    assert!(
        proved
            .allowed_by(RawMask::new(&raw_live))
            .expect("proved coverage check"),
        "suffix memory with matching proof should cover sparse live raw evidence"
    );
}
