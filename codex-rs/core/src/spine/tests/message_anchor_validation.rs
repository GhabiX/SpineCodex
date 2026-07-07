use super::*;

#[test]
fn close_memory_accepts_unknown_user_anchor_reference() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "known user");
    observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_CLOSE, "close");
    let memory = "This memory cites [U99], which may have come from context.".to_string();
    runtime
        .stage_close("close".to_string(), memory.clone())
        .expect("unknown user anchor text should not fast-fail close staging");
    let Some(SpinePendingCommit::Close {
        memory: pending_memory,
        ..
    }) = runtime.pending_commit("close").expect("pending close")
    else {
        panic!("close should stage a pending commit");
    };
    assert_eq!(pending_memory, memory);
}

#[test]
fn next_memory_accepts_unknown_user_anchor_reference() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "known user");
    observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_NEXT, "next");
    let memory = "This sibling memory cites [U99], which may have come from context.".to_string();
    runtime
        .stage_next("next".to_string(), "sibling".to_string(), memory.clone())
        .expect("unknown user anchor text should not fast-fail next staging");
    let Some(SpinePendingCommit::Close {
        memory: pending_memory,
        next_summary,
        ..
    }) = runtime.pending_commit("next").expect("pending next")
    else {
        panic!("next should stage a pending close-family commit");
    };
    assert_eq!(pending_memory, memory);
    assert_eq!(next_summary.as_deref(), Some("sibling"));
}
