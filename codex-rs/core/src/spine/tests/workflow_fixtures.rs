use super::*;
use codex_protocol::spine_tree::SpineTreeNodeSnapshot;
use codex_protocol::spine_tree::SpineTreeUpdateEvent;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

pub(super) fn memory_assembly_with_context_range(
    node_id: &str,
    source_context_range: Range<usize>,
) -> SpineCloseMemoryAssembly {
    let source_raw_range = u64::try_from(source_context_range.start).expect("range start fits u64")
        ..u64::try_from(source_context_range.end).expect("range end fits u64");
    memory_assembly_with_ranges(node_id, source_context_range, source_raw_range)
}

pub(super) fn memory_assembly_with_ranges(
    node_id: &str,
    source_context_range: Range<usize>,
    source_raw_range: Range<u64>,
) -> SpineCloseMemoryAssembly {
    SpineCloseMemoryAssembly {
        body: format!("# Spine Memory {node_id}\n\nreal compact body for {node_id}\n"),
        source_context_range,
        source_raw_range,
        memory_output_tokens: Some(1_250),
    }
}

pub(super) fn observe_spine_request(
    runtime: &mut SpineRuntime,
    raw: &mut Vec<Option<ResponseItem>>,
    tool_name: &str,
    call_id: &str,
) -> (ResponseItem, u64, usize) {
    let request = spine_call(tool_name, call_id);
    let request_ordinal = u64::try_from(raw.len()).expect("raw ordinal fits u64");
    let request_context_index = current_context_len(runtime, raw);
    raw.push(Some(request.clone()));
    runtime.observe_raw_items(1).expect("record spine request");
    runtime
        .observe_context_item(request_ordinal, request_context_index, &request)
        .expect("observe spine request");
    (request, request_ordinal, request_context_index)
}

pub(super) fn observe_function_output(
    runtime: &mut SpineRuntime,
    raw: &mut Vec<Option<ResponseItem>>,
    call_id: &str,
) -> (ResponseItem, u64, usize) {
    let output = function_output(call_id);
    let output_ordinal = u64::try_from(raw.len()).expect("raw ordinal fits u64");
    let output_context_index = current_context_len(runtime, raw)
        .checked_add(1)
        .expect("output context index fits usize");
    raw.push(Some(output.clone()));
    runtime
        .observe_raw_items(1)
        .expect("record function output");
    runtime
        .observe_context_item(output_ordinal, output_context_index, &output)
        .expect("observe function output");
    (output, output_ordinal, output_context_index)
}

pub(super) fn observe_item_at_context_index(
    runtime: &mut SpineRuntime,
    raw: &mut Vec<Option<ResponseItem>>,
    item: ResponseItem,
    context_index: usize,
) -> (ResponseItem, u64, usize) {
    let raw_ordinal = u64::try_from(raw.len()).expect("raw ordinal fits u64");
    raw.push(Some(item.clone()));
    runtime.observe_raw_items(1).expect("record raw item");
    runtime
        .observe_context_item(raw_ordinal, context_index, &item)
        .expect("observe context item");
    (item, raw_ordinal, context_index)
}

// Shared lifecycle and tree projection fixtures.

pub(super) fn open_task(
    runtime: &mut SpineRuntime,
    raw: &mut Vec<Option<ResponseItem>>,
    call_id: &str,
    summary: &str,
) {
    observe_spine_request(runtime, raw, SPINE_TOOL_OPEN, call_id);
    runtime
        .stage_open(call_id.to_string(), summary.to_string())
        .expect("stage open");

    observe_function_output(runtime, raw, call_id);
    runtime
        .maybe_commit_output(call_id, None)
        .expect("commit open");
}

pub(super) fn open_task_with_token_baselines(
    runtime: &mut SpineRuntime,
    raw: &mut Vec<Option<ResponseItem>>,
    call_id: &str,
    summary: &str,
    token_baselines: SpineTokenBaselines,
) {
    observe_spine_request(runtime, raw, SPINE_TOOL_OPEN, call_id);
    runtime
        .stage_open(call_id.to_string(), summary.to_string())
        .expect("stage open");

    observe_function_output(runtime, raw, call_id);
    runtime
        .maybe_commit_output_with_token_baselines(call_id, None, token_baselines)
        .expect("commit open");
}

pub(super) fn append_msg(
    runtime: &mut SpineRuntime,
    raw: &mut Vec<Option<ResponseItem>>,
    text: &str,
) {
    let item = text_item(text);
    let raw_ordinal = u64::try_from(raw.len()).expect("raw ordinal fits u64");
    let context_index = current_context_len(runtime, raw);
    raw.push(Some(item.clone()));
    runtime.observe_raw_items(1).expect("record msg");
    runtime
        .observe_context_item(raw_ordinal, context_index, &item)
        .expect("observe msg");
}

pub(super) fn append_msg_with_context_index(
    runtime: &mut SpineRuntime,
    raw: &mut Vec<Option<ResponseItem>>,
    text: &str,
    context_index: usize,
) {
    let item = text_item(text);
    let raw_ordinal = u64::try_from(raw.len()).expect("raw ordinal fits u64");
    raw.push(Some(item.clone()));
    runtime.observe_raw_items(1).expect("record msg");
    runtime
        .observe_context_item(raw_ordinal, context_index, &item)
        .expect("observe msg");
}

pub(super) fn close_memory_assembly_from_source_plan(
    runtime: &SpineRuntime,
    raw: &[Option<ResponseItem>],
    call_id: &str,
    node_id: &str,
) -> SpineCloseMemoryAssembly {
    let (node, suffix_start) = match runtime
        .pending_commit(call_id)
        .expect("pending close should be readable")
    {
        Some(SpinePendingCommit::Close {
            node, suffix_start, ..
        }) => (node, suffix_start),
        other => panic!("expected pending close, got {other:?}"),
    };
    assert_eq!(node.to_string(), node_id);
    let host_history = runtime
        .materialize_history(raw)
        .expect("materialize host history before pending tool output");
    let toolcall_start = host_history.len();
    let source_plan = runtime
        .build_close_source_plan(&host_history, &node, suffix_start, toolcall_start, call_id)
        .expect("build close source plan");
    memory_assembly_with_ranges(
        node_id,
        source_plan.source_context_range,
        source_plan.source_raw_range,
    )
}

pub(super) fn pending_close_source_plan(
    runtime: &SpineRuntime,
    host_history: &[ResponseItem],
    call_id: &str,
    node_id: &str,
) -> SpineCompactSourcePlan {
    let (node, suffix_start) = match runtime
        .pending_commit(call_id)
        .expect("pending close should be readable")
    {
        Some(SpinePendingCommit::Close {
            node, suffix_start, ..
        }) => (node, suffix_start),
        other => panic!("expected pending close, got {other:?}"),
    };
    assert_eq!(node.to_string(), node_id);
    let toolcall_start = host_history
        .iter()
        .position(|item| matches!(item, ResponseItem::FunctionCall { call_id: existing, .. } if existing == call_id))
        .unwrap_or(host_history.len());
    runtime
        .build_close_source_plan(host_history, &node, suffix_start, toolcall_start, call_id)
        .expect("build close source plan")
}

pub(super) fn close_task(
    runtime: &mut SpineRuntime,
    raw: &mut Vec<Option<ResponseItem>>,
    call_id: &str,
    node_id: &str,
) {
    observe_spine_request(runtime, raw, SPINE_TOOL_CLOSE, call_id);
    runtime
        .stage_close(call_id.to_string(), "test node memory".to_string())
        .expect("stage close");
    let memory_assembly = close_memory_assembly_from_source_plan(runtime, raw, call_id, node_id);

    observe_function_output(runtime, raw, call_id);
    runtime
        .maybe_commit_output(call_id, Some(memory_assembly))
        .expect("commit close");
}

pub(super) fn close_task_with_token_baselines(
    runtime: &mut SpineRuntime,
    raw: &mut Vec<Option<ResponseItem>>,
    call_id: &str,
    node_id: &str,
    token_baselines: SpineTokenBaselines,
) {
    observe_spine_request(runtime, raw, SPINE_TOOL_CLOSE, call_id);
    runtime
        .stage_close(call_id.to_string(), "test node memory".to_string())
        .expect("stage close");
    let memory_assembly = close_memory_assembly_from_source_plan(runtime, raw, call_id, node_id);

    observe_function_output(runtime, raw, call_id);
    runtime
        .maybe_commit_output_with_token_baselines(call_id, Some(memory_assembly), token_baselines)
        .expect("commit close");
}

pub(super) fn next_task(
    runtime: &mut SpineRuntime,
    raw: &mut Vec<Option<ResponseItem>>,
    call_id: &str,
    closing_node_id: &str,
    next_summary: &str,
) -> SpineCommitKind {
    observe_spine_request(runtime, raw, SPINE_TOOL_NEXT, call_id);
    runtime
        .stage_next(
            call_id.to_string(),
            next_summary.to_string(),
            "test node memory".to_string(),
        )
        .expect("stage next");
    let memory_assembly =
        close_memory_assembly_from_source_plan(runtime, raw, call_id, closing_node_id);

    observe_function_output(runtime, raw, call_id);
    runtime
        .maybe_commit_output(call_id, Some(memory_assembly))
        .expect("commit next")
        .expect("next should commit")
}

pub(super) fn next_task_with_token_baselines(
    runtime: &mut SpineRuntime,
    raw: &mut Vec<Option<ResponseItem>>,
    call_id: &str,
    closing_node_id: &str,
    next_summary: &str,
    token_baselines: SpineTokenBaselines,
) -> SpineCommitKind {
    observe_spine_request(runtime, raw, SPINE_TOOL_NEXT, call_id);
    runtime
        .stage_next(
            call_id.to_string(),
            next_summary.to_string(),
            "test node memory".to_string(),
        )
        .expect("stage next");
    let memory_assembly =
        close_memory_assembly_from_source_plan(runtime, raw, call_id, closing_node_id);

    observe_function_output(runtime, raw, call_id);
    runtime
        .maybe_commit_output_with_token_baselines(call_id, Some(memory_assembly), token_baselines)
        .expect("commit next")
        .expect("next should commit")
}

pub(super) fn snapshot_nodes_by_id(
    snapshot: &SpineTreeUpdateEvent,
) -> BTreeMap<&str, &SpineTreeNodeSnapshot> {
    snapshot
        .nodes
        .iter()
        .map(|node| (node.node_id.as_str(), node))
        .collect()
}

pub(super) fn assert_snapshot_is_self_contained_forest(snapshot: &SpineTreeUpdateEvent) {
    let ids = snapshot
        .nodes
        .iter()
        .map(|node| node.node_id.as_str())
        .collect::<BTreeSet<_>>();
    for node in &snapshot.nodes {
        if let Some(parent_id) = node.parent_id.as_deref() {
            assert!(
                ids.contains(parent_id),
                "dangling parent {parent_id} in {snapshot:?}"
            );
        }
    }
}
