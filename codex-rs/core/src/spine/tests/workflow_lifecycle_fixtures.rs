use super::*;

pub(crate) fn open_task(
    runtime: &mut SpineRuntime,
    raw: &mut Vec<Option<ResponseItem>>,
    call_id: &str,
    summary: &str,
) {
    open_task_with_token_baselines(
        runtime,
        raw,
        call_id,
        summary,
        SpineTokenBaselines::default(),
    );
}

pub(crate) fn open_task_with_token_baselines(
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

pub(crate) fn append_msg(
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

pub(crate) fn append_msg_with_context_index(
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

pub(crate) fn close_task(
    runtime: &mut SpineRuntime,
    raw: &mut Vec<Option<ResponseItem>>,
    call_id: &str,
    node_id: &str,
) {
    close_task_with_token_baselines(
        runtime,
        raw,
        call_id,
        node_id,
        SpineTokenBaselines::default(),
    );
}

pub(crate) fn close_task_with_token_baselines(
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

pub(crate) fn next_task(
    runtime: &mut SpineRuntime,
    raw: &mut Vec<Option<ResponseItem>>,
    call_id: &str,
    closing_node_id: &str,
    next_summary: &str,
) -> SpineCommitKind {
    next_task_with_token_baselines(
        runtime,
        raw,
        call_id,
        closing_node_id,
        next_summary,
        SpineTokenBaselines::default(),
    )
}

pub(crate) fn next_task_with_token_baselines(
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
