use super::*;

pub(crate) fn close_memory_assembly_from_source_plan(
    runtime: &SpineRuntime,
    raw: &[Option<ResponseItem>],
    call_id: &str,
    node_id: &str,
) -> SpineCloseMemoryAssembly {
    let host_history = runtime
        .materialize_history_for_test(raw)
        .expect("materialize host history before pending tool output");
    let source_plan =
        pending_close_source_plan_at(runtime, &host_history, call_id, node_id, host_history.len());
    memory_assembly_with_ranges(
        node_id,
        source_plan.source_context_range,
        source_plan.source_raw_range,
    )
}

pub(crate) fn pending_close_source_plan(
    runtime: &SpineRuntime,
    host_history: &[ResponseItem],
    call_id: &str,
    node_id: &str,
) -> SpineCompactSourcePlan {
    let toolcall_start = host_history
        .iter()
        .position(|item| matches!(item, ResponseItem::FunctionCall { call_id: existing, .. } if existing == call_id))
        .unwrap_or(host_history.len());
    pending_close_source_plan_at(runtime, host_history, call_id, node_id, toolcall_start)
}

fn pending_close_source_plan_at(
    runtime: &SpineRuntime,
    host_history: &[ResponseItem],
    call_id: &str,
    node_id: &str,
    toolcall_start: usize,
) -> SpineCompactSourcePlan {
    let pending = runtime
        .pending_commit(call_id)
        .expect("pending close should be readable");
    let Some(SpinePendingCommit::Close {
        node, suffix_start, ..
    }) = pending
    else {
        panic!("expected pending close, got {pending:?}");
    };
    assert_eq!(node.to_string(), node_id);
    runtime
        .build_close_source_plan(host_history, &node, suffix_start, toolcall_start, call_id)
        .expect("build close source plan")
}
