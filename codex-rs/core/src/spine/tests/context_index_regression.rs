use super::*;

const HOLE_COUNT: usize = 6;

fn observe_request_only_hole_item(
    runtime: &mut SpineRuntime,
    raw: &mut Vec<Option<ResponseItem>>,
    next_context_index: &mut usize,
    call_id: &str,
) {
    let request = ordinary_call("shell_command", call_id);
    let raw_ordinal = u64::try_from(raw.len()).expect("raw ordinal fits u64");
    let context_index = *next_context_index;
    *next_context_index = next_context_index
        .checked_add(1)
        .expect("context index fits usize");
    raw.push(Some(request.clone()));
    runtime.observe_raw_items(1).expect("record request raw");
    runtime
        .observe_context_item(raw_ordinal, context_index, &request)
        .expect("observe request-only anchor");
}

fn push_raw_item(
    runtime: &mut SpineRuntime,
    raw: &mut Vec<Option<ResponseItem>>,
    next_context_index: &mut usize,
    item: ResponseItem,
) -> (ResponseItem, u64, usize) {
    let raw_ordinal = u64::try_from(raw.len()).expect("raw ordinal fits u64");
    let context_index = *next_context_index;
    *next_context_index = next_context_index
        .checked_add(1)
        .expect("context index fits usize");
    raw.push(Some(item.clone()));
    runtime.observe_raw_items(1).expect("record raw item");
    (item, raw_ordinal, context_index)
}

fn observe_pushed_raw_item(
    runtime: &mut SpineRuntime,
    raw: &mut Vec<Option<ResponseItem>>,
    next_context_index: &mut usize,
    item: ResponseItem,
) -> (ResponseItem, u64, usize) {
    let (item, raw_ordinal, context_index) = push_raw_item(runtime, raw, next_context_index, item);
    runtime
        .observe_context_item(raw_ordinal, context_index, &item)
        .expect("observe item");
    (item, raw_ordinal, context_index)
}

#[test]
fn reduced_019f024e_request_only_hole_cannot_be_overtaken_by_later_msg() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();
    let mut next_context_index = 0usize;

    // Session 019f024e had raw 480..485 as six durable request-only tool
    // anchors. The root-cause fix makes that shape unrepresentable: a later
    // parser-visible message cannot advance while those requests remain open.
    for index in 0..HOLE_COUNT {
        observe_request_only_hole_item(
            &mut runtime,
            &mut raw,
            &mut next_context_index,
            &format!("hole-{index}"),
        );
    }
    assert_eq!(current_context_len(&runtime, &raw), 0);

    let (first_msg, first_msg_raw, first_msg_context) = push_raw_item(
        &mut runtime,
        &mut raw,
        &mut next_context_index,
        text_item("raw 486 equivalent"),
    );
    let err = runtime
        .observe_context_item(first_msg_raw, first_msg_context, &first_msg)
        .expect_err("later message must not overtake durable request-only hole");
    assert!(
        err.to_string()
            .contains("while durable tool requests are pending"),
        "unexpected overtaking error: {err}"
    );
}

#[test]
fn reduced_019f024e_closed_hole_allows_later_msg_after_completed_toolcall() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();
    let mut next_context_index = 0usize;

    let mut segments = Vec::new();
    for index in 0..HOLE_COUNT {
        let call_id = format!("hole-{index}");
        let (_, request_raw, request_context) = observe_pushed_raw_item(
            &mut runtime,
            &mut raw,
            &mut next_context_index,
            ordinary_call("shell_command", &call_id),
        );
        segments.push(completed_tool_request_segment(request_raw, request_context));
    }
    for index in 0..HOLE_COUNT {
        let call_id = format!("hole-{index}");
        let (_, response_raw, response_context) = observe_pushed_raw_item(
            &mut runtime,
            &mut raw,
            &mut next_context_index,
            function_output(&call_id),
        );
        segments.push(completed_tool_response_segment(
            response_raw,
            response_context,
        ));
    }
    runtime
        .observe_completed_toolcall(CompletedToolCall {
            call_id: "hole-0".to_string(),
            request_call_ids: (0..HOLE_COUNT)
                .map(|index| format!("hole-{index}"))
                .collect(),
            segments,
        })
        .expect("closed request/output group commits as atomic toolcall");

    let toolcall_tail_context = runtime
        .last_visible_response_context_index_for_test()
        .expect("toolcall tail exists");
    assert_eq!(toolcall_tail_context, HOLE_COUNT * 2 - 1);

    let (_, msg_raw, msg_context) = observe_pushed_raw_item(
        &mut runtime,
        &mut raw,
        &mut next_context_index,
        text_item("raw 486 equivalent"),
    );
    assert_eq!(msg_context, HOLE_COUNT * 2);
    assert_eq!(
        msg_context,
        toolcall_tail_context
            .checked_add(1)
            .expect("next context index fits")
    );
    assert_eq!(
        runtime.last_visible_response_context_index_for_test(),
        Some(msg_context)
    );

    assert_eq!(msg_raw, u64::try_from(HOLE_COUNT * 2).expect("raw fits"));
    assert_eq!(
        materialized_trace_signature(&runtime, &raw).last(),
        Some(&"user:raw 486 equivalent".to_string())
    );
}

#[test]
fn reduced_019f024e_stale_rebased_index_is_rejected_after_closed_toolcall() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();
    let mut next_context_index = 0usize;

    let mut grouped_segments = Vec::new();
    for index in 0..8 {
        let call_id = format!("grouped-{index}");
        let (_, request_raw, request_context) = observe_pushed_raw_item(
            &mut runtime,
            &mut raw,
            &mut next_context_index,
            ordinary_call("shell_command", &call_id),
        );
        grouped_segments.push(completed_tool_request_segment(request_raw, request_context));
    }
    for index in 0..8 {
        let call_id = format!("grouped-{index}");
        let (_, output_raw, output_context) = observe_pushed_raw_item(
            &mut runtime,
            &mut raw,
            &mut next_context_index,
            function_output(&call_id),
        );
        grouped_segments.push(completed_tool_response_segment(output_raw, output_context));
    }
    runtime
        .observe_completed_toolcall(CompletedToolCall {
            call_id: "grouped-0".to_string(),
            request_call_ids: (0..8).map(|index| format!("grouped-{index}")).collect(),
            segments: grouped_segments,
        })
        .expect("grouped toolcall advances PS");

    let last_visible_context_index = runtime
        .last_visible_response_context_index_for_test()
        .expect("grouped toolcall tail exists");
    assert_eq!(last_visible_context_index, 15);

    let stale_next_context_index = last_visible_context_index
        .checked_add(1)
        .and_then(|index| index.checked_sub(HOLE_COUNT))
        .expect("stale context index fits");
    let item = text_item("raw 504 stale 187 equivalent");
    let raw_ordinal = u64::try_from(raw.len()).expect("raw ordinal fits u64");
    let stale_err = runtime
        .observe_context_item(raw_ordinal, stale_next_context_index, &item)
        .expect_err("stale rebased context index must not be accepted");
    assert!(
        stale_err
            .to_string()
            .contains("not strictly after previous visible context_index")
            || stale_err
                .to_string()
                .contains("missing spine live rollback checkpoint"),
        "unexpected stale context error: {stale_err}"
    );
    let expected_next_context_index = last_visible_context_index
        .checked_add(1)
        .expect("next context index fits");
    let (_, next_raw, next_context) = observe_pushed_raw_item(
        &mut runtime,
        &mut raw,
        &mut next_context_index,
        text_item("raw 504 equivalent"),
    );
    assert_eq!(next_raw, 16);
    assert_eq!(next_context, expected_next_context_index);
    assert_eq!(
        materialized_trace_signature(&runtime, &raw).last(),
        Some(&"user:raw 504 equivalent".to_string())
    );
}

#[test]
fn close_reduce_tail_uses_mutable_hps_not_closed_child_raw_refs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    for index in 0..3 {
        append_msg_with_context_index(&mut runtime, &mut raw, &format!("parent {index}"), index);
    }

    observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_OPEN, "open-child");
    runtime
        .stage_open("open-child".to_string(), "child".to_string())
        .expect("stage open");
    observe_function_output(&mut runtime, &mut raw, "open-child");
    runtime
        .maybe_commit_output("open-child", None)
        .expect("commit open");

    for index in 0..8 {
        append_msg(&mut runtime, &mut raw, &format!("child item {index}"));
    }

    observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_CLOSE, "close-child");
    runtime
        .stage_close("close-child".to_string(), "child memory".to_string())
        .expect("stage close");
    let memory_assembly =
        close_memory_assembly_from_source_plan(&runtime, &raw, "close-child", "1.1.1");
    observe_function_output(&mut runtime, &mut raw, "close-child");
    runtime
        .maybe_commit_output("close-child", Some(memory_assembly))
        .expect("commit close");

    assert_eq!(
        current_context_len(&runtime, &raw),
        6,
        "parent context should contain prefix, child memory, and close toolcall"
    );
    assert_eq!(
        runtime.last_visible_response_context_index_for_test(),
        Some(5),
        "closed child raw refs must not define the current mutable h(PS) tail"
    );

    let (_, next_raw, next_context) =
        observe_item_at_context_index(&mut runtime, &mut raw, text_item("parent after close"), 6);
    assert_eq!(next_context, 6);
    assert_eq!(next_raw, u64::try_from(raw.len() - 1).expect("raw fits"));
    assert_eq!(
        runtime.last_visible_response_context_index_for_test(),
        Some(6)
    );
}

fn completed_tool_request_segment(
    raw_ordinal: u64,
    context_index: usize,
) -> CompletedToolCallSegment {
    completed_toolcall_segment(ToolCallSegmentKind::Request, raw_ordinal, context_index)
}

fn completed_tool_response_segment(
    raw_ordinal: u64,
    context_index: usize,
) -> CompletedToolCallSegment {
    completed_toolcall_segment(ToolCallSegmentKind::Response, raw_ordinal, context_index)
}

fn completed_toolcall_segment(
    kind: ToolCallSegmentKind,
    raw_ordinal: u64,
    context_index: usize,
) -> CompletedToolCallSegment {
    CompletedToolCallSegment {
        kind,
        raw_ordinal,
        context_index,
    }
}
