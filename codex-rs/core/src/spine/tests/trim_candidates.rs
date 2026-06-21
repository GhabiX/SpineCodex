use super::*;

#[test]
fn completed_toolcall_tags_long_text_tool_response_for_next_turn_trim() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "long-tool");
    let long_text = "x".repeat(600);
    let output = function_output_text("long-tool", &long_text);
    let raw = vec![Some(request.clone()), Some(output.clone())];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(2).expect("record raw");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe request");
    runtime
        .observe_context_item(1, 1, &output)
        .expect("observe output");
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("long-tool", vec![tool_req(0, 0), tool_resp(1, 1)]),
            &raw,
        )
        .expect("observe completed toolcall");

    let rendered = runtime.materialize_history(&raw).expect("materialize");
    assert_eq!(rendered[0], request);
    assert!(
        function_output_text_content(&rendered[1]).starts_with("[TRIM_ID: trim_0]\n"),
        "rendered long output should expose trim id: {:?}",
        rendered[1]
    );
    assert!(
        function_output_text_content(&rendered[1]).contains(&long_text),
        "tagging must keep original visible output until trim"
    );
    assert_eq!(function_output_text_content(&output), long_text);
    let trim_events = runtime.store.trim_events().expect("trim events");
    assert!(matches!(
        trim_events.as_slice(),
        [LoggedTrimEvent {
            trim_seq: 0,
            event: TrimEvent::Candidate {
                trim_id,
                toolcall_seq: 2,
                raw_ordinal: 1,
                context_index: 1,
                call_id,
                response_kind: TrimResponseKind::FunctionCallOutput,
                ..
            }
        }] if trim_id == "trim_0" && call_id == "long-tool"
    ));
}

#[test]
fn completed_toolcall_tags_long_custom_tool_response_for_next_turn_trim() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("custom_tool", "custom-long-tool");
    let long_text = "custom output ".repeat(60);
    let output = custom_tool_output_text("custom-long-tool", &long_text);
    let raw = vec![Some(request.clone()), Some(output.clone())];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(2).expect("record raw");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe request");
    runtime
        .observe_context_item(1, 1, &output)
        .expect("observe output");
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("custom-long-tool", vec![tool_req(0, 0), tool_resp(1, 1)]),
            &raw,
        )
        .expect("observe completed toolcall");

    let rendered = runtime.materialize_history(&raw).expect("materialize");
    assert_eq!(rendered[0], request);
    assert!(
        custom_tool_output_text_content(&rendered[1]).starts_with("[TRIM_ID: trim_0]\n"),
        "rendered custom output should expose trim id: {:?}",
        rendered[1]
    );
    assert!(
        custom_tool_output_text_content(&rendered[1]).contains(&long_text),
        "tagging must keep original custom output visible until trim"
    );
    assert_eq!(custom_tool_output_text_content(&output), long_text);
    let trim_events = runtime.store.trim_events().expect("trim events");
    assert!(matches!(
        trim_events.as_slice(),
        [LoggedTrimEvent {
            trim_seq: 0,
            event: TrimEvent::Candidate {
                trim_id,
                toolcall_seq: 2,
                raw_ordinal: 1,
                context_index: 1,
                call_id,
                response_kind: TrimResponseKind::CustomToolCallOutput,
                ..
            }
        }] if trim_id == "trim_0" && call_id == "custom-long-tool"
    ));
}

#[test]
fn completed_toolcall_does_not_tag_content_items_tool_response_for_trim() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "content-items-tool");
    let output = function_output_content_items("content-items-tool", &"content item ".repeat(80));
    let raw = vec![Some(request.clone()), Some(output.clone())];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(2).expect("record raw");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe request");
    runtime
        .observe_context_item(1, 1, &output)
        .expect("observe output");
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("content-items-tool", vec![tool_req(0, 0), tool_resp(1, 1)]),
            &raw,
        )
        .expect("observe completed toolcall");

    assert_eq!(
        runtime.materialize_history(&raw).expect("materialize"),
        vec![request, output]
    );
    assert!(runtime.store.trim_events().expect("trim events").is_empty());
}

#[test]
fn completed_toolcall_does_not_tag_short_tool_response() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "short-tool");
    let output = function_output("short-tool");
    let raw = vec![Some(request.clone()), Some(output.clone())];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(2).expect("record raw");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe request");
    runtime
        .observe_context_item(1, 1, &output)
        .expect("observe output");
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("short-tool", vec![tool_req(0, 0), tool_resp(1, 1)]),
            &raw,
        )
        .expect("observe completed toolcall");

    assert_eq!(
        runtime.materialize_history(&raw).expect("materialize"),
        vec![request, output]
    );
    assert!(runtime.store.trim_events().expect("trim events").is_empty());
}
