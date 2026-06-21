use super::*;

#[test]
fn trim_slice_head_rewrites_visible_projection_and_preserves_raw_output() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "long-tool");
    let long_text = "abcdefg ".repeat(80);
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

    assert_eq!(
        runtime
            .slice_tool_response_head("trim_0", 7, &raw)
            .expect("slice succeeds"),
        SpineTrimOutcome::Sliced {
            trim_id: "trim_0".to_string()
        }
    );
    let rendered = runtime.materialize_history(&raw).expect("materialize");
    assert_eq!(function_output_text_content(&rendered[1]), "abcdefg");
    assert_eq!(function_output_text_content(&output), long_text);
    assert!(matches!(
        runtime.store.trim_events().expect("persisted trim events").as_slice(),
        [
            LoggedTrimEvent {
                event: TrimEvent::Candidate { trim_id, .. },
                ..
            },
            LoggedTrimEvent {
                event: TrimEvent::Sliced { trim_id: sliced_id, .. },
                ..
            }
        ] if trim_id == "trim_0" && sliced_id == "trim_0"
    ));

    let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw, &[])
        .expect("load replayed runtime")
        .expect("runtime exists");
    let replayed_rendered = replayed
        .materialize_history(&raw)
        .expect("materialize replay");
    assert_eq!(
        function_output_text_content(&replayed_rendered[1]),
        "abcdefg"
    );
}

#[test]
fn trim_slice_tail_rewrites_visible_projection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "long-tool");
    let long_text = format!("{}TAIL-END", "prefix ".repeat(90));
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

    runtime
        .slice_tool_response_tail("trim_0", 8, &raw)
        .expect("slice succeeds");
    let rendered = runtime.materialize_history(&raw).expect("materialize");
    assert_eq!(function_output_text_content(&rendered[1]), "TAIL-END");
}

#[test]
fn trim_slice_anchor_window_rewrites_visible_projection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "long-tool");
    let long_text = format!(
        "{}abc<needle>xyz{}",
        "left ".repeat(60),
        " right".repeat(60)
    );
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

    runtime
        .slice_tool_response_anchor("trim_0", "<needle>", 3, 3, &raw)
        .expect("slice succeeds");
    let rendered = runtime.materialize_history(&raw).expect("materialize");
    assert_eq!(function_output_text_content(&rendered[1]), "abc<needle>xyz");
}

#[test]
fn trim_slice_rejects_missing_anchor_without_projection_change() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "long-tool");
    let long_text = "important raw output ".repeat(40);
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

    assert!(matches!(
        runtime.slice_tool_response_anchor("trim_0", "missing", 1, 1, &raw),
        Ok(SpineTrimOutcome::Miss { trim_id }) if trim_id == "trim_0"
    ));
    let rendered = runtime.materialize_history(&raw).expect("materialize");
    assert!(
        function_output_text_content(&rendered[1]).starts_with("[TRIM_ID: trim_0]\n"),
        "missing anchor must not change visible projection"
    );
}

#[test]
fn trim_repeated_slice_applies_to_current_visible_projection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "long-tool");
    let long_text = format!("{}abcdef{}", "prefix ".repeat(60), " suffix".repeat(60));
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

    runtime
        .slice_tool_response_anchor("trim_0", "abcdef", 0, 0, &raw)
        .expect("first slice succeeds");
    runtime
        .slice_tool_response_head("trim_0", 3, &raw)
        .expect("second slice succeeds");
    let rendered = runtime.materialize_history(&raw).expect("materialize");
    assert_eq!(function_output_text_content(&rendered[1]), "abc");

    let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw, &[])
        .expect("load replayed runtime")
        .expect("runtime exists");
    let replayed_rendered = replayed
        .materialize_history(&raw)
        .expect("materialize replay");
    assert_eq!(function_output_text_content(&replayed_rendered[1]), "abc");
}

#[test]
fn trim_snip_after_slice_clears_visible_body() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "long-tool");
    let long_text = "important raw output ".repeat(40);
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

    runtime
        .slice_tool_response_head("trim_0", 9, &raw)
        .expect("slice succeeds");
    assert_eq!(
        runtime.trim_tool_response("trim_0").expect("snip succeeds"),
        SpineTrimOutcome::Cleared {
            trim_id: "trim_0".to_string()
        }
    );
    let rendered = runtime.materialize_history(&raw).expect("materialize");
    assert_eq!(
        function_output_text_content(&rendered[1]),
        crate::spine::model::TOOL_RESULT_CLEARED_MESSAGE
    );
}
