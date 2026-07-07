use super::*;
use crate::spine::io::hash_raw_live;

#[test]
fn close_retry_reuses_matching_prepared_memory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(0, 0, &spine_call(SPINE_TOOL_OPEN, "open"))
        .expect("observe open request");
    runtime
        .stage_open("open".to_string(), "child".to_string())
        .expect("stage open");
    runtime.observe_raw_items(1).expect("record open output");
    runtime
        .observe_context_item(1, 1, &function_output("open"))
        .expect("observe open output");
    runtime
        .maybe_commit_output("open", None)
        .expect("commit open");
    runtime.observe_raw_items(1).expect("record child raw");
    runtime
        .observe_context_item(2, 2, &text_item("inside"))
        .expect("observe child raw");
    runtime.observe_raw_items(1).expect("record close request");
    runtime
        .observe_context_item(3, 3, &spine_call(SPINE_TOOL_CLOSE, "close"))
        .expect("observe close request");
    runtime
        .stage_close("close".to_string(), "test node memory".to_string())
        .expect("stage close");
    let suffix_start = match runtime.pending_commit("close").expect("pending close") {
        Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
        other => panic!("expected pending close, got {other:?}"),
    };
    let close_request_index = 3;
    runtime.observe_raw_items(1).expect("record close output");
    runtime
        .observe_context_item(4, 4, &function_output("close"))
        .expect("observe close output");

    let memory_assembly =
        memory_assembly_with_context_range("1.1.1", suffix_start..close_request_index);
    let compact_id = "mem-1-1-1-0-3";
    let prepared_mem = suffix_mem_record(
        compact_id,
        NodeId(vec![1, 1, 1]),
        &memory_assembly.body,
        format!("memory/{compact_id}.md"),
        0..3,
        suffix_start..close_request_index,
        memory_assembly.memory_output_tokens,
    );
    let prepared_mem = MemRecord {
        raw_live_hash: Some(hash_raw_live(&[true, true, true])),
        ..prepared_mem
    };
    runtime
        .store
        .write_memory_body(&prepared_mem.compact_id, &memory_assembly.body)
        .expect("write prepared memory body");
    runtime
        .store
        .append_mem(&prepared_mem)
        .expect("append prepared mem");

    let commit = runtime
        .maybe_commit_output("close", Some(memory_assembly))
        .expect("retry close with matching prepared memory")
        .expect("close should commit");
    assert!(matches!(commit, SpineCommitKind::Close { .. }));
    assert_eq!(
        runtime.store.mems().expect("read mems after retry").len(),
        1,
        "retry must reuse matching suffix mem instead of appending duplicate"
    );
    assert_eq!(
        runtime
            .store
            .commit_markers()
            .expect("read commit markers")
            .len(),
        1,
        "retry should publish the explicit close commit proof"
    );
    assert!(
        runtime
            .pending_commit("close")
            .expect("pending close")
            .is_none(),
        "successful retry must clear pending close"
    );
}
