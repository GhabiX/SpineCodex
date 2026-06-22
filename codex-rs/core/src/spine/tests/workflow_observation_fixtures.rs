use super::*;

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
