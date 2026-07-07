use super::*;

#[test]
fn clone_for_rollout_fails_closed_when_visible_memory_body_is_missing() {
    assert_clone_for_rollout_fails_closed_when_visible_memory_body_is_missing();
}

pub(super) fn assert_clone_for_rollout_fails_closed_when_visible_memory_body_is_missing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source_rollout = dir.path().join("source.jsonl");
    let target_rollout = dir.path().join("target.jsonl");
    let source = SpineStore::create_for_rollout(&source_rollout).expect("create source store");
    source
        .append_event(&SpineLedgerEvent::Init { raw_start: 0 })
        .expect("append init");
    let node = NodeId::root_epoch(1).child(1);
    source
        .append_event(&root_child_open_event("root"))
        .expect("append open");
    source
        .append_event(&user_msg_event(0, 0))
        .expect("append msg");
    let close_seq = source
        .append_event(&SpineLedgerEvent::Close {
            node: node.clone(),
            boundary: 3,
            summary: "closed".to_string(),
            close_input_tokens: None,
            close_context_tokens: None,
        })
        .expect("append close");
    source
        .append_event(&manual_toolcall_event(1, 1, 2, 2))
        .expect("append close carrier toolcall");
    let body = "missing body";
    let mem = suffix_mem_record(
        "mem-missing",
        node.clone(),
        body,
        "memory/mem-missing.md".to_string(),
        0..1,
        0..1,
        None,
    );
    source.append_mem(&mem).expect("append missing mem ref");
    source
        .append_commit_marker(&SpineCommitMarker {
            version: COMMIT_MARKER_VERSION,
            op_id: "missing-body-close".to_string(),
            kind: SpineCommitKindMarker::Close,
            token_seq_start: close_seq,
            token_seq_end: close_seq + 2,
            raw_boundary: 3,
            raw_live_hash: None,
            memory_refs: vec![SpineCommitMemoryRef {
                compact_id: mem.compact_id.clone(),
                kind: mem.kind,
                node: mem.node.clone(),
                raw_start: mem.raw_start,
                raw_end: mem.raw_end,
                context_start: mem.context_start,
                context_end: mem.context_end,
                rendered_context_item_count: mem.rendered_context_item_count,
                raw_live_hash: mem.raw_live_hash.clone(),
                body_path: mem.body_path.clone(),
                body_hash: mem.body_hash.clone(),
            }],
        })
        .expect("append close commit marker");

    let boundary = SpineStore::clone_boundary_for_rollout(&source_rollout, 3)
        .expect("capture clone boundary")
        .expect("source sidecar exists");
    let err = SpineStore::clone_for_rollout_with_raw_live(
        &boundary,
        &target_rollout,
        &[true, true, true],
    )
    .expect_err("missing visible memory body must fail closed");
    assert!(
        err.to_string().contains("No such file") || err.to_string().contains("os error 2"),
        "unexpected clone error: {err}"
    );
    assert!(
        !SpineStore::has_for_rollout(&target_rollout).expect("check unpublished target"),
        "failed clone must not publish the target locator"
    );

    let restored_body_path = source
        .write_memory_body(&mem.compact_id, body)
        .expect("restore missing body");
    assert_eq!(restored_body_path, mem.body_path);
    SpineStore::clone_for_rollout_with_raw_live(&boundary, &target_rollout, &[true, true, true])
        .expect("retry clone after restoring missing body");
    let target = SpineStore::for_rollout(&target_rollout).expect("target store after retry");
    let target_mems = target.mems().expect("target mems after retry");
    assert_eq!(
        target_mems
            .iter()
            .map(|record| record.compact_id.as_str())
            .collect::<Vec<_>>(),
        vec!["mem-missing"]
    );
    assert_eq!(
        target
            .read_memory_body(&target_mems[0])
            .expect("read cloned memory body"),
        body
    );
    assert_eq!(
        target
            .commit_markers()
            .expect("target commit markers")
            .len(),
        1
    );
}
